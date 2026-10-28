//! 10.1.1 -- The accessibility tree (spec 16.1).
//!
//! ## A tree beside the render tree, not inside it
//!
//! 16.1 asks for "an accessibility tree **parallel** to the render tree".
//! The word is the design. The render tree of `surface.rs` holds
//! rectangles, lines and glyphs: things with a position and a colour, and
//! no meaning. A screen reader needs the opposite -- what a thing *is*,
//! what it is called, whether it can be reached -- and none of that can
//! be recovered from pixels. So an application declares it, and it
//! declares it separately.
//!
//! Keeping the two apart has a second consequence, and it is the one that
//! makes the design worth it: a module that is paused, a compositor that
//! is not painting, a login screen with no session -- none of them stop
//! the tree being correct. 16.4 wants the shortcuts to work "from the
//! login screen"; a tree that lived inside the render tree would be empty
//! there.
//!
//! ## What a node carries
//!
//! 16.1 lists it: role, label, hint, value, focusable, shortcuts. All six
//! are here and none is optional in the structure -- an absent hint is an
//! empty hint, not a missing field, so that a reader never has to guess
//! whether silence means "nothing to say" or "not filled in".
//!
//! ## The order is the reading order
//!
//! Children are kept in the order they were added, and traversal is
//! depth-first. That is what makes Tab predictable: the next element is
//! the next one a person would read, not the next one in memory. An
//! ordering by screen position would seem more natural and would be
//! wrong -- two columns of a form read column by column, not row by row.
//!
//! ## Bounds
//!
//! The tree is fed by applications, so its size is not ours to choose.
//! `MAX_NODES` is what stops a loop in a client from eating the heap, and
//! every traversal is bounded by the node count rather than by a
//! termination argument: a tree built from outside must be assumed to
//! contain a cycle, whatever the API promises.

use alloc::string::String;
use alloc::vec::Vec;

/// Ceiling on the number of nodes held at once.
pub const MAX_NODES: usize = 256;

/// Identifier of a node. Zero is never a node: it means "none".
pub type NodeId = u32;

/// Role of an element, as a reader announces it (16.1).
///
/// Deliberately short. A role exists to be **spoken**, so every entry
/// here has to be worth hearing before a label; a taxonomy of forty roles
/// would mostly produce noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Window,
    Panel,
    Button,
    Text,
    Field,
    List,
    ListItem,
    CheckBox,
    Menu,
    MenuItem,
    Notification,
}

impl Role {
    /// What the reader says. French, like everything the system utters.
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Window => "fenetre",
            Role::Panel => "panneau",
            Role::Button => "bouton",
            Role::Text => "texte",
            Role::Field => "champ",
            Role::List => "liste",
            Role::ListItem => "element",
            Role::CheckBox => "case",
            Role::Menu => "menu",
            Role::MenuItem => "entree",
            Role::Notification => "notification",
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Role::Window),
            1 => Some(Role::Panel),
            2 => Some(Role::Button),
            3 => Some(Role::Text),
            4 => Some(Role::Field),
            5 => Some(Role::List),
            6 => Some(Role::ListItem),
            7 => Some(Role::CheckBox),
            8 => Some(Role::Menu),
            9 => Some(Role::MenuItem),
            10 => Some(Role::Notification),
            _ => None,
        }
    }

    pub fn code(&self) -> u8 {
        match self {
            Role::Window => 0,
            Role::Panel => 1,
            Role::Button => 2,
            Role::Text => 3,
            Role::Field => 4,
            Role::List => 5,
            Role::ListItem => 6,
            Role::CheckBox => 7,
            Role::Menu => 8,
            Role::MenuItem => 9,
            Role::Notification => 10,
        }
    }

    /// Is an element of this role usually reachable by Tab?
    ///
    /// A default, not a rule: a node says for itself whether it is
    /// focusable. Static text is not, a button is -- and a panel is,
    /// because F6 moves between panels (16.4) and a panel nobody can
    /// reach is a panel F6 skips.
    pub fn focusable_by_default(&self) -> bool {
        matches!(
            self,
            Role::Button
                | Role::Field
                | Role::CheckBox
                | Role::ListItem
                | Role::MenuItem
                | Role::Panel
        )
    }
}

/// One element of the tree (16.1).
#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub parent: NodeId,
    pub role: Role,
    /// Name spoken first after the role.
    pub label: String,
    /// Longer explanation, spoken on demand.
    pub hint: String,
    /// Current value: the text of a field, the state of a box.
    pub value: String,
    pub focusable: bool,
    /// Keyboard shortcuts this element answers to, as written text.
    pub shortcuts: Vec<String>,
    /// Task owning the element, so a dead owner takes its nodes with it.
    pub owner: crate::process::Pid,
    children: Vec<NodeId>,
}

impl Node {
    /// What a reader announces for this node.
    ///
    /// Role first, then label, then value: the role tells the listener
    /// what kind of thing is coming, which is what lets them stop
    /// listening early. Announcing the label first reads better on paper
    /// and is worse in the ear.
    pub fn announcement(&self) -> String {
        let mut out = String::from(self.role.as_str());
        if !self.label.is_empty() {
            out.push(' ');
            out.push_str(&self.label);
        }
        if !self.value.is_empty() {
            out.push_str(", ");
            out.push_str(&self.value);
        }
        if !self.shortcuts.is_empty() {
            out.push_str(", raccourci ");
            out.push_str(&self.shortcuts.join(" ou "));
        }
        out
    }
}

/// The tree itself.
///
/// A flat vector plus parent and child identifiers, rather than boxed
/// nodes: a traversal must never recurse on data that comes from outside,
/// and phase 3 already paid for that lesson in `fs`. The invariant kept
/// here is that every traversal is bounded by `nodes.len()`.
#[derive(Debug, Default)]
pub struct Tree {
    nodes: Vec<Node>,
    next_id: NodeId,
    /// Node holding the focus, zero for none.
    focus: NodeId,
    /// Number of nodes refused because the tree was full.
    pub refused: usize,
}

impl Tree {
    pub const fn new() -> Self {
        Tree {
            nodes: Vec::new(),
            next_id: 1,
            focus: 0,
            refused: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|node| node.id == id)
    }

    pub fn iter(&self) -> core::slice::Iter<'_, Node> {
        self.nodes.iter()
    }

    /// Adds a node under a parent. Zero means "at the root".
    ///
    /// Returns `None` when the tree is full or the parent does not exist.
    /// Attaching to a parent that has gone is not an error worth
    /// panicking over -- a window may close while its children are still
    /// being declared -- but it must not create an orphan the traversal
    /// would then never reach.
    pub fn add(
        &mut self,
        parent: NodeId,
        role: Role,
        label: &str,
        owner: crate::process::Pid,
    ) -> Option<NodeId> {
        if self.nodes.len() >= MAX_NODES {
            self.refused += 1;
            return None;
        }
        if parent != 0 && self.get(parent).is_none() {
            return None;
        }

        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);

        self.nodes.push(Node {
            id,
            parent,
            role,
            label: String::from(label),
            hint: String::new(),
            value: String::new(),
            focusable: role.focusable_by_default(),
            shortcuts: Vec::new(),
            owner,
            children: Vec::new(),
        });

        if parent != 0
            && let Some(node) = self.get_mut(parent)
        {
            node.children.push(id);
        }
        Some(id)
    }

    /// Removes a node and everything under it.
    ///
    /// The descendants are gathered first, then removed: mutating the
    /// vector while walking it is the shape in which a subtree gets half
    /// deleted, and half a subtree is worse than none -- the survivors
    /// are unreachable and still counted.
    pub fn remove(&mut self, id: NodeId) -> usize {
        let doomed = self.subtree(id);
        if doomed.is_empty() {
            return 0;
        }
        if doomed.contains(&self.focus) {
            self.focus = 0;
        }
        if let Some(parent) = self.get(id).map(|node| node.parent)
            && parent != 0
            && let Some(node) = self.get_mut(parent)
        {
            node.children.retain(|child| *child != id);
        }
        let before = self.nodes.len();
        self.nodes.retain(|node| !doomed.contains(&node.id));
        before - self.nodes.len()
    }

    /// Removes everything a task declared.
    ///
    /// Called when a task dies: a tree keeping the nodes of a closed
    /// window would make Tab land on things that are no longer on screen,
    /// and the reader would announce them.
    pub fn forget_owner(&mut self, owner: crate::process::Pid) -> usize {
        let doomed: Vec<NodeId> = self
            .nodes
            .iter()
            .filter(|node| node.owner == owner)
            .map(|node| node.id)
            .collect();
        let mut removed = 0;
        for id in doomed {
            removed += self.remove(id);
        }
        removed
    }

    /// Identifiers of a node and of all its descendants.
    ///
    /// Iterative and bounded by the node count. The bound is not
    /// belt-and-braces: the parent links come from outside, so a cycle
    /// must be assumed possible whatever the API promises.
    pub fn subtree(&self, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        if self.get(root).is_none() {
            return out;
        }
        let mut stack = alloc::vec![root];
        while let Some(id) = stack.pop() {
            if out.contains(&id) || out.len() >= MAX_NODES {
                continue;
            }
            out.push(id);
            if let Some(node) = self.get(id) {
                for child in node.children.iter().rev() {
                    stack.push(*child);
                }
            }
        }
        out
    }

    /// Reading order: depth-first, children in declaration order.
    pub fn reading_order(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let roots: Vec<NodeId> = self
            .nodes
            .iter()
            .filter(|node| node.parent == 0)
            .map(|node| node.id)
            .collect();
        for root in roots {
            for id in self.subtree(root) {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        out
    }

    /// Focusable elements, in reading order (16.4, Tab).
    pub fn focus_order(&self) -> Vec<NodeId> {
        self.reading_order()
            .into_iter()
            .filter(|id| self.get(*id).map(|node| node.focusable).unwrap_or(false))
            .collect()
    }

    pub fn focus(&self) -> NodeId {
        self.focus
    }

    pub fn focused(&self) -> Option<&Node> {
        self.get(self.focus)
    }

    /// Sets the focus, refusing a node that cannot take it.
    pub fn set_focus(&mut self, id: NodeId) -> bool {
        match self.get(id) {
            Some(node) if node.focusable => {
                self.focus = id;
                true
            }
            _ => false,
        }
    }

    /// Moves the focus to the next or previous focusable element.
    ///
    /// It **wraps**: Tab on the last element goes back to the first. That
    /// is not a convenience, it is what stops a keyboard-only user
    /// reaching a dead end from which no key moves anything.
    pub fn move_focus(&mut self, forward: bool) -> Option<NodeId> {
        let order = self.focus_order();
        if order.is_empty() {
            return None;
        }
        let position = order.iter().position(|id| *id == self.focus);
        let next = match (position, forward) {
            (Some(index), true) => order[(index + 1) % order.len()],
            (Some(index), false) => order[(index + order.len() - 1) % order.len()],
            // No focus yet: forward starts at the beginning, backward at
            // the end. Both are the element a person expects to reach.
            (None, true) => order[0],
            (None, false) => order[order.len() - 1],
        };
        self.focus = next;
        Some(next)
    }

    /// Moves to the next panel (16.4, F6).
    ///
    /// A panel and not just any node: F6 exists precisely because Tab
    /// through a hundred fields is not navigation. Landing on the panel
    /// itself rather than on its first child keeps the two keys
    /// independent -- F6 then Tab is "this panel, from the top".
    pub fn next_panel(&mut self) -> Option<NodeId> {
        let panels: Vec<NodeId> = self
            .reading_order()
            .into_iter()
            .filter(|id| self.get(*id).map(|node| node.role == Role::Panel).unwrap_or(false))
            .collect();
        if panels.is_empty() {
            return None;
        }
        // The panel containing the focus, so F6 leaves the one we are in
        // rather than the one that happens to be first.
        let current = self.enclosing_panel(self.focus);
        let next = match current.and_then(|id| panels.iter().position(|panel| *panel == id)) {
            Some(index) => panels[(index + 1) % panels.len()],
            None => panels[0],
        };
        self.focus = next;
        Some(next)
    }

    /// Nearest ancestor of role `Panel`, the node itself included.
    fn enclosing_panel(&self, from: NodeId) -> Option<NodeId> {
        let mut current = from;
        // Bounded by the node count: the parent links come from outside.
        for _ in 0..self.nodes.len().max(1) {
            let node = self.get(current)?;
            if node.role == Role::Panel {
                return Some(node.id);
            }
            if node.parent == 0 {
                return None;
            }
            current = node.parent;
        }
        None
    }

    /// Empties the tree.
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.focus = 0;
    }
}
