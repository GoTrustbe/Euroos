//! EuroWeb DOM — the tree structure that the tree-construction phase builds up.
//!
//! Deliberately simple and ownership-clear: a single `Vec<Node>` arena, children referenced
//! via index (`NodeId`). No `Rc`/`RefCell`, no `unsafe`. This is enough for the
//! style/layout phases that follow (see the [`crate`] module doc).

use alloc::string::String;
use alloc::vec::Vec;

/// Index into the DOM arena. The document root is always `NodeId(0)`.
pub type NodeId = usize;

/// One attribute on an element (`name="value"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    pub name: String,
    pub value: String,
}

/// The kind of node. In practice HTML has a handful of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// The document root (the implicit container above `<html>`).
    Document,
    /// A DOCTYPE declaration, with the name (`html`).
    Doctype(String),
    /// An element: tag name (lowercased) + attributes.
    Element { name: String, attrs: Vec<Attr> },
    /// Text content.
    Text(String),
    /// A `<!-- ... -->` comment.
    Comment(String),
}

/// One node in the arena: its kind, its parent and its children (in order).
#[derive(Debug, Clone)]
pub struct Node {
    pub kind: NodeKind,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

impl Node {
    fn new(kind: NodeKind, parent: Option<NodeId>) -> Self {
        Node { kind, parent, children: Vec::new() }
    }
}

/// The DOM tree: an arena of nodes. `nodes[0]` is the [`NodeKind::Document`] root.
#[derive(Debug, Clone)]
pub struct Dom {
    pub nodes: Vec<Node>,
}

impl Default for Dom {
    fn default() -> Self {
        Self::new()
    }
}

impl Dom {
    /// Create an empty tree with only the document root.
    pub fn new() -> Self {
        Dom { nodes: alloc::vec![Node::new(NodeKind::Document, None)] }
    }

    /// The root id (always 0).
    pub fn root(&self) -> NodeId {
        0
    }

    /// Append `kind` as the last child of `parent`; returns the new id.
    pub fn append(&mut self, parent: NodeId, kind: NodeKind) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node::new(kind, Some(parent)));
        self.nodes[parent].children.push(id);
        id
    }

    /// The tag name of an element node, or `None` for other kinds.
    pub fn tag(&self, id: NodeId) -> Option<&str> {
        match &self.nodes[id].kind {
            NodeKind::Element { name, .. } => Some(name.as_str()),
            _ => None,
        }
    }

    /// Read an attribute value from an element node.
    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        if let NodeKind::Element { attrs, .. } = &self.nodes[id].kind {
            attrs.iter().find(|a| a.name == name).map(|a| a.value.as_str())
        } else {
            None
        }
    }

    /// All text under a node, concatenated in document order.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out, 0);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String, depth: usize) {
        // Depth limit: stop descending on extremely nested trees
        // (anti stack-overflow on malicious input).
        if depth >= 256 {
            return;
        }
        match &self.nodes[id].kind {
            NodeKind::Text(t) => out.push_str(t),
            _ => {
                for &c in &self.nodes[id].children {
                    self.collect_text(c, out, depth + 1);
                }
            }
        }
    }

    /// Number of nodes (including the root). Handy for tests.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Is the tree empty apart from the root?
    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    /// Count the element nodes with a given tag name (depth-first over the whole tree).
    pub fn count_tag(&self, name: &str) -> usize {
        self.nodes
            .iter()
            .filter(|n| matches!(&n.kind, NodeKind::Element { name: t, .. } if t == name))
            .count()
    }
}
