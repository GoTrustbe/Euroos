//! EuroWeb DOM — de boomstructuur die de tree-construction-fase opbouwt.
//!
//! Bewust simpel en eigenaarschap-helder: één `Vec<Node>` arena, kinderen verwijzen
//! via index (`NodeId`). Geen `Rc`/`RefCell`, geen `unsafe`. Dit is genoeg voor de
//! style/layout-fasen die hierop volgen (zie [`crate`]-moduledoc).

use alloc::string::String;
use alloc::vec::Vec;

/// Index in de DOM-arena. De document-root is altijd `NodeId(0)`.
pub type NodeId = usize;

/// Eén attribuut op een element (`naam="waarde"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    pub name: String,
    pub value: String,
}

/// Het soort knoop. HTML kent er in de praktijk een handvol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// De document-root (de impliciete container boven `<html>`).
    Document,
    /// Een DOCTYPE-declaratie, met de naam (`html`).
    Doctype(String),
    /// Een element: tagnaam (lowercased) + attributen.
    Element { name: String, attrs: Vec<Attr> },
    /// Tekst-inhoud.
    Text(String),
    /// Een `<!-- ... -->` commentaar.
    Comment(String),
}

/// Eén knoop in de arena: zijn soort, zijn ouder en zijn kinderen (in volgorde).
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

/// De DOM-boom: een arena van knopen. `nodes[0]` is de [`NodeKind::Document`]-root.
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
    /// Maak een lege boom met enkel de document-root.
    pub fn new() -> Self {
        Dom { nodes: alloc::vec![Node::new(NodeKind::Document, None)] }
    }

    /// De root-id (altijd 0).
    pub fn root(&self) -> NodeId {
        0
    }

    /// Voeg `kind` toe als laatste kind van `parent`; geeft de nieuwe id terug.
    pub fn append(&mut self, parent: NodeId, kind: NodeKind) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node::new(kind, Some(parent)));
        self.nodes[parent].children.push(id);
        id
    }

    /// De tagnaam van een element-knoop, of `None` voor andere soorten.
    pub fn tag(&self, id: NodeId) -> Option<&str> {
        match &self.nodes[id].kind {
            NodeKind::Element { name, .. } => Some(name.as_str()),
            _ => None,
        }
    }

    /// Lees een attribuut-waarde van een element-knoop.
    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        if let NodeKind::Element { attrs, .. } = &self.nodes[id].kind {
            attrs.iter().find(|a| a.name == name).map(|a| a.value.as_str())
        } else {
            None
        }
    }

    /// Alle tekst onder een knoop, in document-volgorde samengevoegd.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out, 0);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String, depth: usize) {
        // Diepte-grens: stop met afdalen op extreem geneste bomen
        // (anti stack-overflow op kwaadwillige invoer).
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

    /// Aantal knopen (inclusief de root). Handig voor tests.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Is de boom leeg op de root na?
    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    /// Tel de element-knopen met een gegeven tagnaam (depth-first over de hele boom).
    pub fn count_tag(&self, name: &str) -> usize {
        self.nodes
            .iter()
            .filter(|n| matches!(&n.kind, NodeKind::Element { name: t, .. } if t == name))
            .count()
    }
}
