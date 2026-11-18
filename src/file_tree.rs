use crate::model::{Kind, Tree};
use std::collections::HashSet;

pub const ROW_HEIGHT: f64 = 23.0;

#[derive(Default)]
pub struct FileTree {
    expanded: HashSet<String>,
    order: Vec<Vec<usize>>,
    pub rows: Vec<usize>,
    revision: Option<u64>,
    selection_revision: Option<u64>,
    pub scroll_y: f64,
    pub scroll_x: f64,
    pub content_width: f64,
    pub focus: Option<usize>,
    pub reveal_pending: bool,
}

pub fn is_folder(kind: Kind) -> bool {
    matches!(kind, Kind::Dir | Kind::Ghost { dir: true, .. })
}

impl FileTree {
    pub fn sync(&mut self, tree: &Tree, revision: u64, selected: Option<usize>, selection_revision: u64) {
        let changed = self.revision != Some(revision);
        if changed {
            if self.revision.is_none() { self.expanded.insert(String::new()); }
            self.order = tree.nodes.iter().map(|node| {
                let mut children: Vec<_> = node.children.iter().copied().filter(|&i| tree.is_visible(i)).collect();
                children.sort_by(|&a, &b| {
                    let a = &tree.nodes[a];
                    let b = &tree.nodes[b];
                    is_folder(b.kind).cmp(&is_folder(a.kind))
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                        .then_with(|| a.name.cmp(&b.name))
                });
                children
            }).collect();
            self.revision = Some(revision);
        }
        let reveal = self.selection_revision != Some(selection_revision);
        self.selection_revision = Some(selection_revision);
        if reveal {
            if let Some(index) = selected.filter(|&i| tree.is_visible(i)) {
                let mut parent = tree.nodes[index].parent;
                while let Some(i) = parent {
                    self.expanded.insert(tree.nodes[i].path.clone());
                    parent = tree.nodes[i].parent;
                }
                self.focus = Some(index);
                self.reveal_pending = true;
            }
        }
        if changed || reveal { self.rebuild(tree); }
        if self.focus.is_some_and(|index| !self.rows.contains(&index)) {
            self.focus = selected.filter(|index| self.rows.contains(index)).or(self.rows.first().copied());
        }
    }

    fn rebuild(&mut self, tree: &Tree) {
        self.rows.clear();
        let mut stack = if tree.is_visible(0) { vec![0] } else { Vec::new() };
        while let Some(index) = stack.pop() {
            self.rows.push(index);
            if self.is_expanded(tree, index) {
                stack.extend(self.order[index].iter().rev());
            }
        }
        self.content_width = self.rows.iter().map(|&i| {
            let node = &tree.nodes[i];
            node.depth as f64*16.0 + node.name.chars().count() as f64*12.0 + 100.0
        }).fold(0.0, f64::max);
    }

    pub fn is_expanded(&self, tree: &Tree, index: usize) -> bool {
        self.expanded.contains(&tree.nodes[index].path)
    }

    pub fn toggle(&mut self, tree: &Tree, index: usize) {
        if !is_folder(tree.nodes[index].kind) { return; }
        let path = &tree.nodes[index].path;
        if !self.expanded.remove(path) { self.expanded.insert(path.clone()); }
        self.rebuild(tree);
        if self.focus.is_some_and(|focus| !self.rows.contains(&focus)) { self.focus = Some(index); }
    }

    pub fn visible_range(&mut self, tree: &Tree, width: f64, height: f64) -> std::ops::Range<usize> {
        let max_scroll = (self.rows.len() as f64 * ROW_HEIGHT - height).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
        if self.reveal_pending {
            if let Some(node) = self.focus.and_then(|index| tree.nodes.get(index)) {
                let label_x = 29.0 + node.depth as f64 * 16.0;
                if label_x < self.scroll_x + 21.0 || label_x + 80.0_f64.min((width-32.0).max(1.0)) > self.scroll_x + width {
                    self.scroll_x = node.depth.saturating_sub(1) as f64 * 16.0;
                }
            }
            if let Some(row) = self.focus.and_then(|index| self.rows.iter().position(|&i| i == index)) {
                let top = row as f64 * ROW_HEIGHT;
                if top < self.scroll_y { self.scroll_y = top; }
                else if top + ROW_HEIGHT > self.scroll_y + height { self.scroll_y = (top + ROW_HEIGHT - height).clamp(0.0, max_scroll); }
            }
            self.reveal_pending = false;
        }
        let first = (self.scroll_y / ROW_HEIGHT).floor() as usize;
        first.min(self.rows.len())..((self.scroll_y + height) / ROW_HEIGHT).ceil().min(self.rows.len() as f64) as usize
    }

    pub fn adjacent(&self, delta: isize) -> Option<usize> {
        if self.rows.is_empty() { return None; }
        let row = self.focus.and_then(|i| self.rows.iter().position(|&index| index == i)).unwrap_or(0);
        Some(self.rows[row.saturating_add_signed(delta).min(self.rows.len()-1)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{filter::Filter, scan::{Entry, EntryKind, Line}};
    fn file(tree: &mut Tree, path: &str) {
        tree.insert(Entry { path:path.into(), ignored:false, kind:EntryKind::Text{bytes:10,lines:vec![Line::default()]} });
    }
    fn paths(browser: &FileTree, tree: &Tree) -> Vec<String> {
        browser.rows.iter().map(|&i|tree.nodes[i].path.clone()).collect()
    }
    #[test]
    fn folders_sort_before_files_and_nested_expansion_survives_collapse() {
        let mut tree=Tree::new("project");
        for path in ["z.rb","b/z.rb","A/nested/file.rb","a.rb"] {file(&mut tree,path);}
        tree.update_weights(false);
        let mut browser=FileTree::default();browser.sync(&tree,1,None,0);
        assert_eq!(paths(&browser,&tree),["","A","b","a.rb","z.rb"]);
        let a=tree.find("A").unwrap();let nested=tree.find("A/nested").unwrap();
        browser.toggle(&tree,a);browser.toggle(&tree,nested);
        assert!(paths(&browser,&tree).contains(&"A/nested/file.rb".into()));
        browser.toggle(&tree,a);browser.toggle(&tree,a);
        assert!(paths(&browser,&tree).contains(&"A/nested/file.rb".into()));
    }
    #[test]
    fn shared_filters_hide_rows_and_restore_expanded_branches() {
        let mut tree=Tree::new("project");file(&mut tree,"app/a.rb");file(&mut tree,"spec/a_spec.rb");tree.update_weights(false);
        let mut browser=FileTree::default();browser.sync(&tree,1,None,0);
        browser.toggle(&tree,tree.find("app").unwrap());
        tree.apply_filter(&Filter::parse("spec/").unwrap(),false,None);browser.sync(&tree,2,None,0);
        assert_eq!(paths(&browser,&tree),["","spec"]);
        tree.apply_filter(&Filter::default(),true,None);browser.sync(&tree,3,None,0);
        assert_eq!(paths(&browser,&tree),["","app","app/a.rb"]);
    }
    #[test]
    fn ignored_expansion_uses_shared_visibility_and_keeps_the_open_folder() {
        let mut tree=Tree::new("project");file(&mut tree,"app/a.rb");
        tree.insert(Entry{path:"vendor".into(),ignored:true,kind:EntryKind::Ghost{dir:true}});tree.update_weights(false);
        let mut browser=FileTree::default();browser.sync(&tree,1,None,0);assert!(!paths(&browser,&tree).contains(&"vendor".into()));
        tree.update_weights(true);browser.sync(&tree,2,None,0);let vendor=tree.find("vendor").unwrap();browser.toggle(&tree,vendor);
        tree.graft(vendor,vec![Entry{path:"vendor/b.rb".into(),ignored:true,kind:EntryKind::Text{bytes:1,lines:vec![Line::default()]}}]);tree.update_weights(true);browser.sync(&tree,3,None,0);
        assert!(paths(&browser,&tree).contains(&"vendor/b.rb".into()));
        tree.update_weights(false);browser.sync(&tree,4,None,0);assert!(!paths(&browser,&tree).contains(&"vendor".into()));
    }
    #[test]
    fn explicit_selection_reveals_ancestors_but_manual_collapse_is_respected() {
        let mut tree=Tree::new("project");file(&mut tree,"app/models/a.rb");tree.update_weights(false);
        let index=tree.find("app/models/a.rb").unwrap();let app=tree.find("app").unwrap();
        let mut browser=FileTree::default();browser.sync(&tree,1,Some(index),1);
        assert!(browser.rows.contains(&index));assert_eq!(browser.focus,Some(index));
        browser.toggle(&tree,app);browser.sync(&tree,1,Some(index),1);assert!(!browser.rows.contains(&index));
        browser.sync(&tree,1,Some(index),2);assert!(browser.rows.contains(&index));
    }
    #[test]
    fn selecting_a_shallow_file_reveals_its_name_after_horizontal_scroll() {
        let mut tree=Tree::new("project");file(&mut tree,"a/b/c/d/e/f/g/h/i/j/deep.rb");file(&mut tree,"z.txt");tree.update_weights(false);
        let mut browser=FileTree::default();let deep=tree.find("a/b/c/d/e/f/g/h/i/j/deep.rb").unwrap();
        browser.sync(&tree,1,Some(deep),1);browser.visible_range(&tree,120.0,500.0);assert!(browser.scroll_x>0.0);
        let shallow=tree.find("z.txt").unwrap();browser.sync(&tree,1,Some(shallow),2);browser.visible_range(&tree,120.0,500.0);assert_eq!(browser.scroll_x,0.0);
    }
    #[test]
    fn large_expanded_folder_limits_the_viewport_and_reveals_offscreen_selection() {
        let mut tree=Tree::new("project");for i in 0..20_000{file(&mut tree,&format!("app/file_{i:05}.rb"));}tree.update_weights(false);
        let mut browser=FileTree::default();browser.sync(&tree,1,None,0);browser.toggle(&tree,tree.find("app").unwrap());assert_eq!(browser.rows.len(),20_002);
        let index=tree.find("app/file_19999.rb").unwrap();browser.sync(&tree,1,Some(index),1);
        let range=browser.visible_range(&tree,300.0,500.0);assert!(range.len()<=23);assert!(range.contains(&(browser.rows.len()-1)));
        browser.scroll_y=f64::MAX;assert!(browser.visible_range(&tree,300.0,500.0).len()<=23);
        browser.scroll_y=-100.0;assert_eq!(browser.visible_range(&tree,300.0,500.0).start,0);
    }
}
