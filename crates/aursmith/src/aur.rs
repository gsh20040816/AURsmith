use anyhow::{Context, bail};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    os::unix::process::CommandExt,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub const MAX_FILES: usize = 2_048;
pub const MAX_PATH_BYTES: usize = 1_024;
pub const MAX_PATH_COMPONENTS: usize = 32;
pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_TREE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_DIFF_BYTES: usize = 4 * 1024 * 1024;
pub const GIT_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);

const GIT_BINARY: &str = "/usr/bin/git";
const GIT_METADATA_OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const GIT_TREE_LISTING_OUTPUT_LIMIT: usize = MAX_FILES * (MAX_PATH_BYTES + 128);
const GIT_STDERR_LIMIT: usize = 256 * 1024;
const SIGKILL: i32 = 9;

#[derive(Debug)]
struct GitStdoutLimitExceeded {
    limit: usize,
}

impl std::fmt::Display for GitStdoutLimitExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Git stdout 超过固定上限 {} 字节", self.limit)
    }
}

impl std::error::Error for GitStdoutLimitExceeded {}

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub path: String,
    pub mode: u32,
    pub content: Vec<u8>,
}

#[derive(Debug)]
pub struct CompleteTree {
    pub commit: String,
    pub git_tree_oid: String,
    pub tree_sha256: String,
    pub entries: Vec<TreeEntry>,
}

#[derive(Debug)]
pub enum FetchedTree {
    Complete(CompleteTree),
    InputBlocked { commit: String, blocker: String },
}

#[derive(Debug, Clone)]
pub enum AurSource {
    Production,
    #[cfg(test)]
    Fixture(PathBuf),
}

impl AurSource {
    pub fn production() -> Self {
        Self::Production
    }

    #[cfg(test)]
    pub fn fixture(path: PathBuf) -> Self {
        Self::Fixture(path)
    }

    fn remote(&self, pkgbase: &str) -> OsString {
        match self {
            Self::Production => format!("https://aur.archlinux.org/{pkgbase}.git").into(),
            #[cfg(test)]
            Self::Fixture(path) => path.as_os_str().to_owned(),
        }
    }

    fn allows_file_protocol(&self) -> bool {
        match self {
            Self::Production => false,
            #[cfg(test)]
            Self::Fixture(_) => true,
        }
    }
}

pub fn fetch_tree(
    state_root: &Path,
    pkgbase: &str,
    source: &AurSource,
    deadline: Instant,
) -> anyhow::Result<FetchedTree> {
    fs::create_dir_all(state_root)
        .with_context(|| format!("无法创建 AUR 状态目录 {}", state_root.display()))?;
    let package_root = state_root.join(pkgbase);
    fs::create_dir_all(&package_root)
        .with_context(|| format!("无法创建 pkgbase 状态目录 {}", package_root.display()))?;
    let repository = package_root.join("repository.git");
    let home = state_root.join(".git-home");
    fs::create_dir_all(&home)
        .with_context(|| format!("无法创建隔离 Git HOME {}", home.display()))?;
    ensure_bare_repository(&repository, &home, source, deadline)?;
    run_git(
        Some(&repository),
        &home,
        source,
        [
            OsStr::new("fetch"),
            OsStr::new("--quiet"),
            OsStr::new("--no-tags"),
            OsStr::new("--force"),
            OsStr::new("--depth=1"),
            OsStr::new("--no-recurse-submodules"),
            source.remote(pkgbase).as_os_str(),
            OsStr::new("HEAD"),
        ],
        GIT_METADATA_OUTPUT_LIMIT,
        deadline,
    )?;
    let commit = read_object_id(
        run_git(
            Some(&repository),
            &home,
            source,
            [
                OsStr::new("rev-parse"),
                OsStr::new("--verify"),
                OsStr::new("FETCH_HEAD^{commit}"),
            ],
            128,
            deadline,
        )?,
        "commit",
    )?;
    read_tree_at_commit(&repository, &home, source, &commit, deadline)
}

pub fn read_tree_at_commit(
    repository: &Path,
    home: &Path,
    source: &AurSource,
    commit: &str,
    deadline: Instant,
) -> anyhow::Result<FetchedTree> {
    validate_object_id(commit, "commit")?;
    let tree_oid = read_object_id(
        run_git(
            Some(repository),
            home,
            source,
            [
                OsStr::new("rev-parse"),
                OsStr::new("--verify"),
                OsStr::new(&format!("{commit}^{{tree}}")),
            ],
            128,
            deadline,
        )?,
        "tree",
    )?;
    let listing = match run_git(
        Some(repository),
        home,
        source,
        [
            OsStr::new("ls-tree"),
            OsStr::new("-r"),
            OsStr::new("-z"),
            OsStr::new("--full-tree"),
            OsStr::new(&tree_oid),
        ],
        GIT_TREE_LISTING_OUTPUT_LIMIT,
        deadline,
    ) {
        Ok(listing) => listing,
        Err(error) if git_stdout_exceeded(&error) => {
            return Ok(FetchedTree::InputBlocked {
                commit: commit.to_owned(),
                blocker: format!(
                    "Git tree listing 超过固定上限 {GIT_TREE_LISTING_OUTPUT_LIMIT} 字节"
                ),
            });
        }
        Err(error) => return Err(error),
    };
    let metadata = match parse_tree_listing(&listing) {
        Ok(metadata) => metadata,
        Err(blocker) => {
            return Ok(FetchedTree::InputBlocked {
                commit: commit.to_owned(),
                blocker,
            });
        }
    };
    if let Err(blocker) = validate_file_count(metadata.len()) {
        return Ok(FetchedTree::InputBlocked {
            commit: commit.to_owned(),
            blocker,
        });
    }

    let mut entries = Vec::with_capacity(metadata.len());
    let mut total_size = 0usize;
    for (path, mode, oid) in metadata {
        let size_output = run_git(
            Some(repository),
            home,
            source,
            [OsStr::new("cat-file"), OsStr::new("-s"), OsStr::new(&oid)],
            64,
            deadline,
        )?;
        let size = parse_decimal_size(&size_output)
            .with_context(|| format!("Git blob {oid} 返回非法大小"))?;
        total_size = match checked_tree_size(total_size, size, &path) {
            Ok(total_size) => total_size,
            Err(blocker) => {
                return Ok(FetchedTree::InputBlocked {
                    commit: commit.to_owned(),
                    blocker,
                });
            }
        };
        let content = run_git(
            Some(repository),
            home,
            source,
            [OsStr::new("cat-file"), OsStr::new("blob"), OsStr::new(&oid)],
            size,
            deadline,
        )?;
        if content.len() != size {
            bail!("Git blob {oid} 的内容长度与声明不一致");
        }
        entries.push(TreeEntry {
            path,
            mode,
            content,
        });
    }
    entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let tree_sha256 = canonical_tree_sha256(&entries);
    Ok(FetchedTree::Complete(CompleteTree {
        commit: commit.to_owned(),
        git_tree_oid: tree_oid,
        tree_sha256,
        entries,
    }))
}

#[cfg(not(test))]
pub fn diff_trees(
    repository: &Path,
    home: &Path,
    source: &AurSource,
    baseline_tree_oid: &str,
    current_tree_oid: &str,
    deadline: Instant,
) -> anyhow::Result<Option<Vec<u8>>> {
    diff_trees_with_limit(
        repository,
        home,
        source,
        baseline_tree_oid,
        current_tree_oid,
        deadline,
        MAX_DIFF_BYTES,
    )
}

#[cfg(test)]
pub fn diff_trees_for_test(
    repository: &Path,
    home: &Path,
    source: &AurSource,
    baseline_tree_oid: &str,
    current_tree_oid: &str,
    deadline: Instant,
    limit: usize,
) -> anyhow::Result<Option<Vec<u8>>> {
    diff_trees_with_limit(
        repository,
        home,
        source,
        baseline_tree_oid,
        current_tree_oid,
        deadline,
        limit,
    )
}

fn diff_trees_with_limit(
    repository: &Path,
    home: &Path,
    source: &AurSource,
    baseline_tree_oid: &str,
    current_tree_oid: &str,
    deadline: Instant,
    limit: usize,
) -> anyhow::Result<Option<Vec<u8>>> {
    validate_object_id(baseline_tree_oid, "baseline tree")?;
    validate_object_id(current_tree_oid, "current tree")?;
    match run_git(
        Some(repository),
        home,
        source,
        [
            OsStr::new("diff"),
            OsStr::new("--binary"),
            OsStr::new("--full-index"),
            OsStr::new("--no-ext-diff"),
            OsStr::new("--no-textconv"),
            OsStr::new("--no-renames"),
            OsStr::new("--no-color"),
            OsStr::new(baseline_tree_oid),
            OsStr::new(current_tree_oid),
        ],
        limit,
        deadline,
    ) {
        Ok(diff) => Ok(Some(diff)),
        Err(error) if git_stdout_exceeded(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn repository_path(state_root: &Path, pkgbase: &str) -> PathBuf {
    state_root.join(pkgbase).join("repository.git")
}

pub fn git_home_path(state_root: &Path) -> PathBuf {
    state_root.join(".git-home")
}

pub fn materialize_tree(root: &Path, entries: &[TreeEntry]) -> anyhow::Result<String> {
    fs::create_dir(root)
        .with_context(|| format!("无法创建 package 临时目录 {}", root.display()))?;
    for entry in entries {
        validate_relative_path(entry.path.as_bytes()).map_err(anyhow::Error::msg)?;
        let target = root.join(&entry.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建 package 子目录 {}", parent.display()))?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(entry.mode)
            .open(&target)
            .with_context(|| format!("无法创建 package 文件 {}", target.display()))?;
        file.write_all(&entry.content)
            .with_context(|| format!("无法写入 package 文件 {}", target.display()))?;
        fs::set_permissions(&target, fs::Permissions::from_mode(entry.mode))
            .with_context(|| format!("无法固定 package 文件权限 {}", target.display()))?;
        file.sync_all()
            .with_context(|| format!("无法同步 package 文件 {}", target.display()))?;
    }
    sync_directory_tree(root)?;
    let disk_entries = read_materialized_tree(root)?;
    let expected = canonical_tree_sha256(entries);
    let actual = canonical_tree_sha256(&disk_entries);
    if expected != actual {
        bail!("package 物化后 tree SHA-256 不一致");
    }
    Ok(actual)
}

fn sync_directory_tree(root: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(root)
        .with_context(|| format!("无法读取待同步 package 目录 {}", root.display()))?
    {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            sync_directory_tree(&entry.path())?;
        }
    }
    File::open(root)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("无法同步 package 目录 {}", root.display()))
}

pub fn verify_materialized_tree(root: &Path, expected_sha256: &str) -> anyhow::Result<()> {
    let entries = read_materialized_tree(root)?;
    let actual = canonical_tree_sha256(&entries);
    if actual != expected_sha256 {
        bail!("package tree SHA-256 与审查记录不一致");
    }
    Ok(())
}

pub fn canonical_tree_sha256(entries: &[TreeEntry]) -> String {
    let mut ordered = entries.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let mut digest = Sha256::new();
    digest.update(b"aursmith-tree-v1\0");
    for entry in ordered {
        digest.update((entry.path.len() as u64).to_be_bytes());
        digest.update(entry.path.as_bytes());
        digest.update(entry.mode.to_be_bytes());
        digest.update((entry.content.len() as u64).to_be_bytes());
        digest.update(&entry.content);
    }
    hex::encode(digest.finalize())
}

fn ensure_bare_repository(
    repository: &Path,
    home: &Path,
    source: &AurSource,
    deadline: Instant,
) -> anyhow::Result<()> {
    match fs::symlink_metadata(repository) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            return Ok(());
        }
        Ok(_) => bail!("Git 缓存路径不是普通目录：{}", repository.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("无法检查 Git 缓存路径"),
    }
    let mut random = [0u8; 16];
    OsRng.fill_bytes(&mut random);
    let staging = repository.with_file_name(format!(".repository-{}.tmp", hex::encode(random)));
    let result = run_git(
        None,
        home,
        source,
        [
            OsStr::new("init"),
            OsStr::new("--quiet"),
            OsStr::new("--bare"),
            staging.as_os_str(),
        ],
        GIT_METADATA_OUTPUT_LIMIT,
        deadline,
    )
    .and_then(|_| {
        fs::rename(&staging, repository).with_context(|| {
            format!(
                "无法原子安装 Git 缓存 {} -> {}",
                staging.display(),
                repository.display()
            )
        })
    });
    if result.is_err() && staging.exists() {
        fs::remove_dir_all(&staging)
            .with_context(|| format!("无法清理本次 Git 临时目录 {}", staging.display()))?;
    }
    result
}

fn run_git<I, S>(
    repository: Option<&Path>,
    home: &Path,
    source: &AurSource,
    arguments: I,
    stdout_limit: usize,
    deadline: Instant,
) -> anyhow::Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    if Instant::now() >= deadline {
        bail!("Git 总超时已耗尽");
    }
    let mut command = Command::new(GIT_BINARY);
    command
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS", "/bin/false")
        .env("GIT_PROTOCOL_FROM_USER", "0")
        .env("LANG", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);
    if let Some(repository) = repository {
        command.arg("--git-dir").arg(repository);
    }
    command
        .args(["-c", "credential.helper="])
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(["-c", "submodule.recurse=false"])
        .args(["-c", "protocol.allow=never"])
        .args(["-c", "protocol.ext.allow=never"])
        .args(["-c", "protocol.https.allow=always"]);
    if source.allows_file_protocol() {
        command.args(["-c", "protocol.file.allow=always"]);
    }
    command.args(arguments);
    let mut child = command.spawn().context("无法启动固定 Git 二进制")?;
    let stdout = child.stdout.take().context("无法读取 Git stdout")?;
    let stderr = child.stderr.take().context("无法读取 Git stderr")?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout, stdout_limit));
    let stderr_reader = thread::spawn(move || read_bounded(stderr, GIT_STDERR_LIMIT));

    let mut exit_status = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => exit_status = Some(status),
            Ok(None) => {}
            Err(error) => {
                let cleanup = terminate_process_group(&mut child);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                cleanup.context("Git wait 异常后无法终止进程组")?;
                return Err(error).context("无法等待 Git 子进程");
            }
        }
        if exit_status.is_some() && stdout_reader.is_finished() && stderr_reader.is_finished() {
            break;
        }
        if Instant::now() >= deadline {
            terminate_process_group(&mut child).context("Git 超时后无法回收进程组")?;
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            bail!("Git 操作超过固定总超时 {} 秒", GIT_TOTAL_TIMEOUT.as_secs());
        }
        thread::sleep(Duration::from_millis(10));
    }
    let status = exit_status.context("Git reader 已结束但缺少子进程退出状态")?;
    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("Git stdout 读取线程异常退出"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("Git stderr 读取线程异常退出"))??;
    if stdout.overflowed {
        return Err(GitStdoutLimitExceeded {
            limit: stdout_limit,
        }
        .into());
    }
    if stderr.overflowed {
        bail!("Git stderr 超过固定上限 {GIT_STDERR_LIMIT} 字节");
    }
    if !status.success() {
        bail!(
            "Git 命令失败（{}）：{}",
            status,
            String::from_utf8_lossy(&stderr.bytes).trim()
        );
    }
    Ok(stdout.bytes)
}

fn terminate_process_group(child: &mut std::process::Child) -> anyhow::Result<()> {
    let process_group = i32::try_from(child.id()).context("Git PID 超出 Unix pid_t 范围")?;
    // SAFETY: every caller starts the child with process_group(0), making the child's PID the
    // process-group ID. A negative PID therefore targets only that isolated Git process group.
    let result = unsafe { kill(-process_group, SIGKILL) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(3) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("无法向 Git 进程组发送 SIGKILL");
        }
    }
    child.wait().context("无法 wait Git 进程组 leader")?;
    Ok(())
}

struct BoundedOutput {
    bytes: Vec<u8>,
    overflowed: bool,
}

fn read_bounded(mut reader: impl Read, limit: usize) -> std::io::Result<BoundedOutput> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0u8; 16 * 1024];
    let mut overflowed = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let retained = remaining.min(read);
        bytes.extend_from_slice(&buffer[..retained]);
        overflowed |= retained < read;
    }
    Ok(BoundedOutput { bytes, overflowed })
}

fn parse_tree_listing(listing: &[u8]) -> Result<Vec<(String, u32, String)>, String> {
    let mut result = Vec::new();
    let mut previous_path: Option<Vec<u8>> = None;
    for record in listing
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| "Git tree 记录缺少路径分隔符".to_owned())?;
        let (metadata, path_with_separator) = record.split_at(separator);
        let path = &path_with_separator[1..];
        let mut fields = metadata.split(|byte| *byte == b' ');
        let mode = fields
            .next()
            .ok_or_else(|| "Git tree 缺少 mode".to_owned())?;
        let kind = fields
            .next()
            .ok_or_else(|| "Git tree 缺少对象类型".to_owned())?;
        let oid = fields
            .next()
            .ok_or_else(|| "Git tree 缺少对象 ID".to_owned())?;
        if fields.next().is_some() {
            return Err("Git tree 元数据字段数量非法".to_owned());
        }
        if kind != b"blob" || !matches!(mode, b"100644" | b"100755") {
            return Err(format!(
                "tracked tree 只接受 100644/100755 普通 blob，拒绝 mode={} type={}",
                String::from_utf8_lossy(mode),
                String::from_utf8_lossy(kind)
            ));
        }
        validate_relative_path(path)?;
        if previous_path
            .as_deref()
            .is_some_and(|previous| previous >= path)
        {
            return Err("Git tree 路径重复或顺序非法".to_owned());
        }
        previous_path = Some(path.to_vec());
        let path = std::str::from_utf8(path)
            .map_err(|_| "tracked tree 路径不是 UTF-8".to_owned())?
            .to_owned();
        let oid = std::str::from_utf8(oid)
            .map_err(|_| "Git blob ID 不是 ASCII".to_owned())?
            .to_owned();
        validate_object_id(&oid, "blob").map_err(|error| error.to_string())?;
        result.push((path, if mode == b"100755" { 0o755 } else { 0o644 }, oid));
    }
    Ok(result)
}

fn validate_relative_path(path: &[u8]) -> Result<(), String> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return Err(format!(
            "tracked path 长度必须在 1 至 {MAX_PATH_BYTES} 字节之间"
        ));
    }
    let path = std::str::from_utf8(path).map_err(|_| "tracked path 不是 UTF-8".to_owned())?;
    if path.chars().any(char::is_control) {
        return Err("tracked path 包含控制字符".to_owned());
    }
    let parsed = Path::new(path);
    if parsed.is_absolute() {
        return Err("tracked path 不得为绝对路径".to_owned());
    }
    let components = parsed.components().collect::<Vec<_>>();
    if components.is_empty() || components.len() > MAX_PATH_COMPONENTS {
        return Err(format!("tracked path 层级不得超过 {MAX_PATH_COMPONENTS}"));
    }
    for component in components {
        let Component::Normal(component) = component else {
            return Err("tracked path 不得包含点、点点或逃逸组件".to_owned());
        };
        if component.as_encoded_bytes().len() > 255 {
            return Err("tracked path 单个组件超过 255 字节".to_owned());
        }
    }
    Ok(())
}

fn read_materialized_tree(root: &Path) -> anyhow::Result<Vec<TreeEntry>> {
    let root_metadata = fs::symlink_metadata(root)
        .with_context(|| format!("无法检查 package 根目录 {}", root.display()))?;
    if !root_metadata.file_type().is_dir() || root_metadata.file_type().is_symlink() {
        bail!("package 根路径不是普通目录");
    }
    let mut pending = vec![PathBuf::new()];
    let mut files = BTreeMap::new();
    let mut total_size = 0usize;
    while let Some(relative_directory) = pending.pop() {
        let directory = root.join(&relative_directory);
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("无法读取 package 目录 {}", directory.display()))?
        {
            let entry = entry?;
            let relative = relative_directory.join(entry.file_name());
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                bail!("package tree 包含符号链接");
            }
            if metadata.file_type().is_dir() {
                pending.push(relative);
                continue;
            }
            if !metadata.file_type().is_file() {
                bail!("package tree 包含非普通文件");
            }
            if files.len() >= MAX_FILES {
                bail!("package tree 文件数超过固定上限");
            }
            let path = relative
                .to_str()
                .context("package tree 路径不是 UTF-8")?
                .to_owned();
            validate_relative_path(path.as_bytes()).map_err(anyhow::Error::msg)?;
            let size = usize::try_from(metadata.len()).context("package 文件大小无法表示")?;
            if size > MAX_FILE_BYTES {
                bail!("package 文件超过固定单文件上限");
            }
            total_size = total_size.checked_add(size).context("package 总大小溢出")?;
            if total_size > MAX_TREE_BYTES {
                bail!("package tree 超过固定总大小上限");
            }
            let mode = metadata.permissions().mode() & 0o777;
            if !matches!(mode, 0o644 | 0o755) {
                bail!("package 文件权限不是 0644/0755");
            }
            let mut content = Vec::with_capacity(size);
            File::open(entry.path())?.read_to_end(&mut content)?;
            if content.len() != size {
                bail!("package 文件读取长度发生变化");
            }
            files.insert(
                path.clone(),
                TreeEntry {
                    path,
                    mode,
                    content,
                },
            );
        }
    }
    Ok(files.into_values().collect())
}

fn read_object_id(output: Vec<u8>, label: &str) -> anyhow::Result<String> {
    let value = std::str::from_utf8(&output)
        .with_context(|| format!("Git {label} ID 不是 ASCII"))?
        .trim();
    validate_object_id(value, label)?;
    Ok(value.to_owned())
}

fn validate_object_id(value: &str, label: &str) -> anyhow::Result<()> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("Git {label} ID 不是 40 位小写十六进制 SHA-1");
    }
    Ok(())
}

fn parse_decimal_size(output: &[u8]) -> anyhow::Result<usize> {
    let value = std::str::from_utf8(output)?.trim();
    value.parse::<usize>().context("无法解析十进制大小")
}

fn git_stdout_exceeded(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<GitStdoutLimitExceeded>().is_some())
}

fn validate_file_count(count: usize) -> Result<(), String> {
    if count > MAX_FILES {
        return Err(format!("tracked tree 文件数超过固定上限 {MAX_FILES}"));
    }
    Ok(())
}

fn checked_tree_size(current: usize, file_size: usize, path: &str) -> Result<usize, String> {
    if file_size > MAX_FILE_BYTES {
        return Err(format!(
            "文件 {path} 超过固定单文件上限 {MAX_FILE_BYTES} 字节"
        ));
    }
    let total = current
        .checked_add(file_size)
        .ok_or_else(|| "tracked tree 总大小计算溢出".to_owned())?;
    if total > MAX_TREE_BYTES {
        return Err(format!(
            "tracked tree 超过固定总大小上限 {MAX_TREE_BYTES} 字节"
        ));
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, mode: u32, content: &[u8]) -> TreeEntry {
        TreeEntry {
            path: path.to_owned(),
            mode,
            content: content.to_vec(),
        }
    }

    #[test]
    fn paths_reject_escape_non_utf8_controls_and_fixed_bounds() {
        for valid in [b"PKGBUILD".as_slice(), b"src/lib.rs", b"name\\literal"] {
            validate_relative_path(valid).unwrap();
        }
        for invalid in [
            b"".as_slice(),
            b"/absolute",
            b"./dot",
            b"../escape",
            b"nested/../escape",
            b"line\nbreak",
            b"bad\xffutf8",
        ] {
            assert!(validate_relative_path(invalid).is_err(), "{invalid:?}");
        }
        assert!(validate_relative_path("a".repeat(MAX_PATH_BYTES + 1).as_bytes()).is_err());
        let too_deep = std::iter::repeat_n("x", MAX_PATH_COMPONENTS + 1)
            .collect::<Vec<_>>()
            .join("/");
        assert!(validate_relative_path(too_deep.as_bytes()).is_err());
    }

    #[test]
    fn tree_listing_rejects_non_blob_and_special_modes() {
        let oid = "a".repeat(40);
        for record in [
            format!("120000 blob {oid}\tlink\0"),
            format!("160000 commit {oid}\tsubmodule\0"),
            format!("100644 blob {oid}\t../escape\0"),
        ] {
            assert!(parse_tree_listing(record.as_bytes()).is_err());
        }
    }

    #[test]
    fn canonical_tree_is_order_independent_and_covers_mode_path_length_and_content() {
        let first = entry("PKGBUILD", 0o644, b"pkgname=demo\n");
        let second = entry("bin/tool", 0o755, b"#!/bin/sh\n");
        let expected = canonical_tree_sha256(&[first.clone(), second.clone()]);
        assert_eq!(expected, canonical_tree_sha256(&[second, first.clone()]));
        assert_ne!(
            expected,
            canonical_tree_sha256(&[entry("PKGBUILD", 0o755, &first.content)])
        );
        assert_ne!(
            expected,
            canonical_tree_sha256(&[entry("OTHER", 0o644, &first.content)])
        );
        assert_ne!(
            expected,
            canonical_tree_sha256(&[entry("PKGBUILD", 0o644, b"changed")])
        )
    }

    #[test]
    fn materialized_tree_is_stable_and_tampering_is_detected() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("package");
        let entries = vec![
            entry("PKGBUILD", 0o644, b"pkgname=demo\n"),
            entry("bin/tool", 0o755, b"#!/bin/sh\nexit 0\n"),
        ];
        let digest = materialize_tree(&root, &entries).unwrap();
        assert_eq!(digest, canonical_tree_sha256(&entries));
        verify_materialized_tree(&root, &digest).unwrap();

        let mut file = OpenOptions::new()
            .append(true)
            .open(root.join("PKGBUILD"))
            .unwrap();
        file.write_all(b"tampered\n").unwrap();
        assert!(verify_materialized_tree(&root, &digest).is_err());
    }

    #[test]
    fn bounded_reader_drains_but_never_retains_past_the_limit() {
        let output = read_bounded(&b"0123456789"[..], 4).unwrap();
        assert_eq!(output.bytes, b"0123");
        assert!(output.overflowed);
    }

    #[test]
    fn file_count_single_file_and_total_size_bounds_are_exact() {
        validate_file_count(MAX_FILES).unwrap();
        assert!(validate_file_count(MAX_FILES + 1).is_err());
        assert_eq!(
            checked_tree_size(0, MAX_FILE_BYTES, "file").unwrap(),
            MAX_FILE_BYTES
        );
        assert!(checked_tree_size(0, MAX_FILE_BYTES + 1, "file").is_err());
        assert_eq!(
            checked_tree_size(MAX_TREE_BYTES - 1, 1, "file").unwrap(),
            MAX_TREE_BYTES
        );
        assert!(checked_tree_size(MAX_TREE_BYTES, 1, "file").is_err());
    }

    #[test]
    fn tree_listing_cap_covers_the_legal_worst_case_and_overflow_is_classifiable() {
        let legal_worst_case = (0..MAX_FILES).map(|_| MAX_PATH_BYTES + 64).sum::<usize>();
        assert!(
            GIT_TREE_LISTING_OUTPUT_LIMIT >= legal_worst_case,
            "listing cap 必须覆盖合法文件数与路径长度的最坏元数据"
        );
        let error = anyhow::Error::new(GitStdoutLimitExceeded {
            limit: GIT_TREE_LISTING_OUTPUT_LIMIT,
        })
        .context("读取固定 Git tree 失败");
        assert!(git_stdout_exceeded(&error));
    }

    #[test]
    fn process_group_helper_terminates_and_reaps_the_fixed_child_group() {
        let mut command = Command::new("/usr/bin/sleep");
        command.arg("30").process_group(0);
        let mut child = command.spawn().unwrap();
        terminate_process_group(&mut child).unwrap();
        assert!(child.try_wait().unwrap().is_some());
    }
}
