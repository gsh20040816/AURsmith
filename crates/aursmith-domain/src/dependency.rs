use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use thiserror::Error;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyGraph {
    /// 将一个 pkgbase 映射到它依赖的其他 pkgbase。
    dependencies: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GraphError {
    #[error("dependency cycle involves: {0:?}")]
    Cycle(BTreeSet<String>),
    #[error("unknown package base: {0}")]
    UnknownPackage(String),
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
        self.dependencies.entry(dependency.clone()).or_default();
        self.dependencies
            .entry(package_base)
            .or_default()
            .insert(dependency);
    }

    pub fn dependencies_of(&self, package_base: &str) -> Option<&BTreeSet<String>> {
        self.dependencies.get(package_base)
    }

    pub fn topological_order(&self) -> Result<Vec<String>, GraphError> {
        let mut remaining: BTreeMap<_, _> = self
            .dependencies
            .iter()
            .map(|(node, dependencies)| (node.clone(), dependencies.clone()))
            .collect();
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

    /// 返回发生变化的节点、它们的全部反向依赖，以及把这些受影响节点作为
    /// 一个发布批次重建时需要的全部依赖。
    pub fn affected_release_closure(
        &self,
        changed: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, GraphError> {
        if let Some(missing) = changed
            .iter()
            .find(|node| !self.dependencies.contains_key(*node))
        {
            return Err(GraphError::UnknownPackage(missing.clone()));
        }

        let mut affected = changed.clone();
        loop {
            let previous = affected.len();
            for (node, dependencies) in &self.dependencies {
                if dependencies
                    .iter()
                    .any(|dependency| affected.contains(dependency))
                {
                    affected.insert(node.clone());
                }
            }
            if affected.len() == previous {
                break;
            }
        }

        let mut closure = affected;
        loop {
            let previous = closure.len();
            let nodes: Vec<_> = closure.iter().cloned().collect();
            for node in nodes {
                if let Some(dependencies) = self.dependencies.get(&node) {
                    closure.extend(dependencies.iter().cloned());
                }
            }
            if closure.len() == previous {
                break;
            }
        }
        Ok(closure)
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
    fn release_closure_includes_reverse_dependents_and_their_dependencies() {
        let changed = BTreeSet::from(["library".to_owned()]);
        assert_eq!(
            graph().affected_release_closure(&changed).unwrap(),
            BTreeSet::from([
                "app".to_owned(),
                "library".to_owned(),
                "toolchain-addon".to_owned(),
            ])
        );
    }
}
