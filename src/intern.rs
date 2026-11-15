use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::refs::Ref;

/// 名前を番号にした参照。1 件 17 バイトで、名前の文字列を件数ぶん持たない。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackedRef {
    pub name: u32,
    /// 0 は包含元なし。それ以外は名前の番号+1。
    pub container: u32,
    pub line: u32,
    pub col: u32,
    pub kind: u8,
}

#[derive(Default)]
struct Table {
    ids: HashMap<Arc<str>, u32>,
    names: Vec<Arc<str>>,
}

/// 名前の番号表。プロセスで 1 つだけ持ち、名前の種類数（件数ではない）ぶんだけ増える。
fn table() -> &'static Mutex<Table> {
    static T: OnceLock<Mutex<Table>> = OnceLock::new();
    T.get_or_init(Mutex::default)
}

fn locked() -> std::sync::MutexGuard<'static, Table> {
    // 表への操作は途中で panic しない。毒化しても中身は整合している
    table().lock().unwrap_or_else(|e| e.into_inner())
}

pub fn intern(s: &str) -> u32 {
    let mut t = locked();
    t.intern(s)
}

impl Table {
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.ids.get(s) {
            return id;
        }
        let id = self.names.len() as u32;
        let a: Arc<str> = Arc::from(s);
        self.names.push(Arc::clone(&a));
        self.ids.insert(a, id);
        id
    }
}

/// 同じ名前は同じ番号になる。ファイルごとに 1 度だけロックする。
pub fn pack(refs: &[Ref]) -> Vec<PackedRef> {
    let mut local: HashMap<&str, u32> = HashMap::new();
    for s in refs
        .iter()
        .flat_map(|r| [Some(r.name.as_str()), r.container.as_deref()])
        .flatten()
    {
        local.entry(s).or_insert(0);
    }
    {
        let mut t = locked();
        for (s, id) in &mut local {
            *id = t.intern(s);
        }
    }
    refs.iter()
        .map(|r| PackedRef {
            name: local[r.name.as_str()],
            container: r.container.as_deref().map_or(0, |c| local[c] + 1),
            line: r.line,
            col: r.col,
            kind: r.kind,
        })
        .collect()
}

pub fn names_of(ids: &[u32]) -> Vec<Arc<str>> {
    let t = locked();
    ids.iter()
        .map(|&i| Arc::clone(&t.names[i as usize]))
        .collect()
}
