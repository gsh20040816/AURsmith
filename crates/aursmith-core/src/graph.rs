//! pkgbase 依赖图：拓扑排序与从订阅出发的闭包计算。

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use thiserror::Error;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DependencyGraph {
    /// pkgbase -> 它依赖的其他 pkgbase。
    dependencies: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GraphError {
    #[error("依赖环涉及：{0:?}")]
    Cycle(BTreeSet<String>),
}

impl DependencyGraph {
    pub fn add_package(&mut self, package_base: impl Into<String>) {
        self.dependencies.entry(package_base.into()).or_default();
    }

    pub fn add_dependency(
        &mut self,
        package_base: impl Into<String>,
        dependency: impl Into<String>,
    ) {
        let package_base = package_base.into();
        let dependency = dependency.into();
        if package_base == dependency {
            self.add_package(package_base);
            return;
        }
        self.dependencies.entry(dependency.clone()).or_default();
        self.dependencies
            .entry(package_base)
            .or_default()
            .insert(dependency);
    }

    pub fn dependencies_of(&self, package_base: &str) -> impl Iterator<Item = &String> {
        self.dependencies
            .get(package_base)
            .into_iter()
            .flat_map(|dependencies| dependencies.iter())
    }

    pub fn contains(&self, package_base: &str) -> bool {
        self.dependencies.contains_key(package_base)
    }

    /// 依赖先于依赖者；同层按名字排序，结果确定。
    pub fn topological_order(&self) -> Result<Vec<String>, GraphError> {
        let mut remaining = self.dependencies.clone();
        let mut ready: VecDeque<String> = remaining
            .iter()
            .filter(|(_, dependencies)| dependencies.is_empty())
            .map(|(node, _)| node.clone())
            .collect();
        let mut ordered = Vec::with_capacity(remaining.len());
        while let Some(node) = ready.pop_front() {
            if remaining.remove(&node).is_none() {
                continue;
            }
            ordered.push(node.clone());
            for (dependent, dependencies) in &mut remaining {
                if dependencies.remove(&node) && dependencies.is_empty() {
                    ready.push_back(dependent.clone());
                }
            }
        }
        if remaining.is_empty() {
            Ok(ordered)
        } else {
            Err(GraphError::Cycle(remaining.into_keys().collect()))
        }
    }

    /// 从根集合出发沿依赖边可达的全部节点（含根）。
    pub fn closure<'a>(&self, roots: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut queue: VecDeque<String> = roots.into_iter().map(str::to_owned).collect();
        while let Some(node) = queue.pop_front() {
            if !seen.insert(node.clone()) {
                continue;
            }
            for dependency in self.dependencies_of(&node) {
                if !seen.contains(dependency) {
                    queue.push_back(dependency.clone());
                }
            }
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> DependencyGraph {
        let mut graph = DependencyGraph::default();
        graph.add_dependency("app", "library");
        graph.add_dependency("library", "toolchain-addon");
        graph.add_package("unrelated");
        graph
    }

    #[test]
    fn dependencies_are_built_before_dependents() {
        assert_eq!(
            graph().topological_order().unwrap(),
            vec!["toolchain-addon", "unrelated", "library", "app"]
        );
    }

    #[test]
    fn cycles_are_never_silently_ordered() {
        let mut graph = graph();
        graph.add_dependency("toolchain-addon", "app");
        assert!(matches!(
            graph.topological_order(),
            Err(GraphError::Cycle(_))
        ));
    }

    #[test]
    fn closure_follows_dependencies_only() {
        assert_eq!(
            graph().closure(["app"]),
            BTreeSet::from(["app".into(), "library".into(), "toolchain-addon".into()])
        );
        assert_eq!(graph().closure(["library"]).len(), 2);
    }

    #[test]
    fn self_dependency_is_ignored() {
        let mut graph = DependencyGraph::default();
        graph.add_dependency("a", "a");
        assert_eq!(graph.topological_order().unwrap(), vec!["a"]);
    }
}
