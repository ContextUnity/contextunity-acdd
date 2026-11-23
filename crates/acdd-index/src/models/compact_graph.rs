use super::tags::{
    CoverageStatus, EdgeConfidence, EdgeKind, LanguageId, NodeKind, ReferenceKind,
};
use super::types::{
    Coverage as PublicCoverage, Edge as PublicEdge, Graph as PublicGraph, Reference,
};
use serde::{Serialize, Serializer};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum CompactTag {
    Static(&'static str),
    Owned(Box<str>),
}

impl CompactTag {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Static(value) => value,
            Self::Owned(value) => value,
        }
    }

    pub fn known(value: &str) -> Option<&'static str> {
        use std::str::FromStr;
        EdgeKind::from_str(value)
            .ok()
            .map(|v| v.as_str())
            .or_else(|| EdgeConfidence::from_str(value).ok().map(|v| v.as_str()))
            .or_else(|| CoverageStatus::from_str(value).ok().map(|v| v.as_str()))
            .or_else(|| NodeKind::from_str(value).ok().map(|v| v.as_str()))
            .or_else(|| ReferenceKind::from_str(value).ok().map(|v| v.as_str()))
            .or_else(|| LanguageId::from_str(value).ok().map(|v| v.as_str()))
    }

    pub fn into_string(self) -> String {
        match self {
            Self::Static(value) => value.to_owned(),
            Self::Owned(value) => value.into_string(),
        }
    }
}

impl From<CompactTag> for String {
    fn from(tag: CompactTag) -> Self {
        tag.into_string()
    }
}

impl AsRef<str> for CompactTag {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for CompactTag {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl std::hash::Hash for CompactTag {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl From<&str> for CompactTag {
    fn from(value: &str) -> Self {
        Self::known(value).map_or_else(|| Self::Owned(value.into()), Self::Static)
    }
}

impl From<String> for CompactTag {
    fn from(value: String) -> Self {
        Self::known(&value).map_or_else(|| Self::Owned(value.into_boxed_str()), Self::Static)
    }
}

macro_rules! compact_tag_from_wire {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl From<$ty> for CompactTag {
                fn from(value: $ty) -> Self {
                    Self::Static(value.as_str())
                }
            }
        )+
    };
}

compact_tag_from_wire!(
    EdgeKind,
    EdgeConfidence,
    CoverageStatus,
    NodeKind,
    ReferenceKind,
    LanguageId,
);

impl Serialize for CompactTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl PartialEq for CompactTag {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for CompactTag {}

impl PartialOrd for CompactTag {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CompactTag {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialEq<&str> for CompactTag {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

pub(crate) trait SharedStr {
    fn as_str(&self) -> &str;
    fn into_arc(self) -> Arc<str>;
}

impl SharedStr for &str {
    fn as_str(&self) -> &str {
        self
    }

    fn into_arc(self) -> Arc<str> {
        Arc::from(self)
    }
}

impl SharedStr for String {
    fn as_str(&self) -> &str {
        self
    }

    fn into_arc(self) -> Arc<str> {
        Arc::from(self.as_str())
    }
}

impl SharedStr for &String {
    fn as_str(&self) -> &str {
        self
    }

    fn into_arc(self) -> Arc<str> {
        Arc::from(self.as_str())
    }
}

impl SharedStr for Arc<str> {
    fn as_str(&self) -> &str {
        self
    }

    fn into_arc(self) -> Arc<str> {
        self
    }
}

impl SharedStr for &Arc<str> {
    fn as_str(&self) -> &str {
        self
    }

    fn into_arc(self) -> Arc<str> {
        Arc::clone(self)
    }
}

impl SharedStr for Cow<'_, str> {
    fn as_str(&self) -> &str {
        self.as_ref()
    }

    fn into_arc(self) -> Arc<str> {
        Arc::from(self.as_ref())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CompactEdge {
    pub src: Arc<str>,
    pub dst: Arc<str>,
    pub kind: CompactTag,
    pub path: Arc<str>,
    pub line: usize,
    pub evidence: CompactTag,
    pub confidence: CompactTag,
}

impl From<&PublicEdge> for CompactEdge {
    fn from(edge: &PublicEdge) -> Self {
        let path = Arc::from(edge.path.as_str());
        Self::with_path(edge, &path)
    }
}

impl CompactEdge {
    pub fn with_path(edge: &PublicEdge, path: &Arc<str>) -> Self {
        Self {
            src: Arc::from(edge.src.as_str()),
            dst: Arc::from(edge.dst.as_str()),
            kind: edge.kind.as_str().into(),
            path: Arc::clone(path),
            line: edge.line,
            evidence: edge.evidence.as_str().into(),
            confidence: edge.confidence.as_str().into(),
        }
    }
}

impl From<CompactEdge> for PublicEdge {
    fn from(edge: CompactEdge) -> Self {
        Self {
            src: edge.src.as_ref().to_owned(),
            dst: edge.dst.as_ref().to_owned(),
            kind: edge.kind.into_string(),
            path: edge.path.as_ref().to_owned(),
            line: edge.line,
            evidence: edge.evidence.into_string(),
            confidence: edge.confidence.into_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CompactCoverage {
    pub path: Arc<str>,
    pub line: usize,
    pub expression: String,
    pub status: CompactTag,
    pub evidence: Arc<str>,
}

impl From<CompactCoverage> for PublicCoverage {
    fn from(coverage: CompactCoverage) -> Self {
        Self {
            path: coverage.path.as_ref().to_owned(),
            line: coverage.line,
            expression: coverage.expression,
            status: coverage.status.into_string(),
            evidence: coverage.evidence.as_ref().to_owned(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CompactGraph {
    pub edges: Vec<CompactEdge>,
    pub coverage: Vec<CompactCoverage>,
    pub path_arc: Option<Arc<str>>,
    pub path_pool: hashbrown::HashSet<Arc<str>>,
    pub node_id_pool: hashbrown::HashSet<Arc<str>>,
    pub evidence_pool: hashbrown::HashSet<Arc<str>>,
}

impl CompactGraph {
    pub fn intern_arc(pool: &mut hashbrown::HashSet<Arc<str>>, value: Arc<str>) -> Arc<str> {
        if let Some(existing) = pool.get(value.as_ref()) {
            return Arc::clone(existing);
        }
        pool.insert(Arc::clone(&value));
        value
    }

    fn shared_from_pool<S: SharedStr>(
        pool: &mut hashbrown::HashSet<Arc<str>>,
        value: S,
    ) -> Arc<str> {
        if let Some(shared) = pool.get(value.as_str()) {
            return Arc::clone(shared);
        }
        let shared = value.into_arc();
        pool.insert(Arc::clone(&shared));
        shared
    }

    fn shared_path(&mut self, path: &str) -> Arc<str> {
        if let Some(shared) = self
            .path_arc
            .as_ref()
            .filter(|shared| shared.as_ref() == path)
        {
            return Arc::clone(shared);
        }
        Self::shared_from_pool(&mut self.path_pool, path)
    }

    pub fn push_public_edge(&mut self, edge: &PublicEdge) {
        let path = self.shared_path(&edge.path);
        let src = Self::shared_from_pool(&mut self.node_id_pool, edge.src.as_str());
        let dst = Self::shared_from_pool(&mut self.node_id_pool, edge.dst.as_str());
        self.edges.push(CompactEdge {
            src,
            dst,
            kind: edge.kind.as_str().into(),
            path,
            line: edge.line,
            evidence: edge.evidence.as_str().into(),
            confidence: edge.confidence.as_str().into(),
        });
    }

    pub fn merge(&mut self, other: CompactGraph) {
        self.edges.reserve(other.edges.len());
        for mut edge in other.edges {
            edge.src = Self::intern_arc(&mut self.node_id_pool, edge.src);
            edge.dst = Self::intern_arc(&mut self.node_id_pool, edge.dst);
            edge.path = Self::intern_arc(&mut self.path_pool, edge.path);
            self.edges.push(edge);
        }
        self.coverage.reserve(other.coverage.len());
        for mut coverage in other.coverage {
            coverage.path = Self::intern_arc(&mut self.path_pool, coverage.path);
            coverage.evidence = Self::intern_arc(&mut self.evidence_pool, coverage.evidence);
            self.coverage.push(coverage);
        }
    }

    pub fn clear_pools(&mut self) {
        self.path_arc = None;
        self.path_pool = hashbrown::HashSet::new();
        self.node_id_pool = hashbrown::HashSet::new();
        self.evidence_pool = hashbrown::HashSet::new();
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_edge<S, D, K, E, C>(
        &mut self,
        path: &str,
        src: S,
        dst: D,
        kind: K,
        line: usize,
        evidence: E,
        confidence: C,
    ) where
        S: SharedStr,
        D: SharedStr,
        K: Into<CompactTag>,
        E: Into<CompactTag>,
        C: Into<CompactTag>,
    {
        let path = self.shared_path(path);
        let src = Self::shared_from_pool(&mut self.node_id_pool, src);
        let dst = Self::shared_from_pool(&mut self.node_id_pool, dst);
        self.edges.push(CompactEdge {
            src,
            dst,
            kind: kind.into(),
            path,
            line,
            evidence: evidence.into(),
            confidence: confidence.into(),
        });
    }

    pub(crate) fn push_coverage<P, S, E>(
        &mut self,
        path: &str,
        line: usize,
        expression: P,
        status: S,
        evidence: E,
    ) where
        P: Into<String>,
        S: Into<CompactTag>,
        E: SharedStr,
    {
        let path = self.shared_path(path);
        let evidence = Self::shared_from_pool(&mut self.evidence_pool, evidence);
        self.coverage.push(CompactCoverage {
            path,
            line,
            expression: expression.into(),
            status: status.into(),
            evidence,
        });
    }

    pub(crate) fn reference_edge<D: SharedStr, K: Into<CompactTag>, C: Into<CompactTag>>(
        &mut self,
        path: &str,
        reference: &Reference,
        target: D,
        kind: K,
        confidence: C,
    ) {
        self.push_edge(
            path,
            reference.source.as_str(),
            target,
            kind,
            reference.line,
            reference.expression.as_str(),
            confidence,
        );
    }

    pub fn reference_coverage<S: Into<CompactTag>>(
        &mut self,
        path: &str,
        reference: &Reference,
        status: S,
        evidence: Cow<'static, str>,
    ) {
        self.push_coverage(
            path,
            reference.line,
            reference.expression.clone(),
            status,
            evidence,
        );
    }
}

impl From<CompactGraph> for PublicGraph {
    fn from(graph: CompactGraph) -> Self {
        Self {
            edges: graph.edges.into_iter().map(Into::into).collect(),
            coverage: graph.coverage.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_edge_and_push_coverage_preserve_wire_parity() {
        let mut typed_graph = CompactGraph::default();
        typed_graph.push_edge(
            "a.rs",
            "a",
            "b",
            EdgeKind::Calls,
            1,
            "call",
            EdgeConfidence::Exact,
        );
        typed_graph.push_coverage("a.rs", 1, "b", CoverageStatus::Resolved, "call");
        let mut wire_graph = CompactGraph::default();
        wire_graph.push_edge("a.rs", "a", "b", "calls", 1, "call", "exact");
        wire_graph.push_coverage("a.rs", 1, "b", "resolved", "call");
        assert_eq!(typed_graph.edges.len(), 1);
        assert_eq!(typed_graph.edges[0].dst.as_ref(), "b");
        assert_eq!(
            serde_json::to_value(&typed_graph.edges).unwrap(),
            serde_json::to_value(&wire_graph.edges).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&typed_graph.coverage).unwrap(),
            serde_json::to_value(&wire_graph.coverage).unwrap()
        );
    }
}
