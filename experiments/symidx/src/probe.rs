// 使い捨て: 既存索引を FST + varint に変換し、サイズ・逆引きの正しさ・検索時間を測る
use std::{path::Path, time::Instant};

use fst::{Automaton, IntoStreamer, Map, MapBuilder, Streamer, raw};

use crate::{DEF_HDR, FILE_HDR, Res, View, open, u32_at};

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

// 出力（rank）が鍵順に単調増加する FST だけで成り立つ逆引き（Lucene Util.getByOutput と同じ考え方）
pub fn key_by_rank(f: &raw::Fst<Vec<u8>>, id: u64) -> Option<Vec<u8>> {
    let (mut node, mut acc, mut key) = (f.root(), 0u64, Vec::new());
    loop {
        if node.is_final() && acc + node.final_output().value() == id {
            return Some(key);
        }
        let t = node.transitions().take_while(|t| acc + t.out.value() <= id).last()?;
        acc += t.out.value();
        key.push(t.inp);
        node = f.node(t.addr);
    }
}

/// ASCII の大文字小文字を無視する部分文字列 automaton（KMP）。状態 None は一致済み。
pub struct Substr {
    pat: Vec<u8>,
    fail: Vec<usize>,
}

impl Substr {
    pub fn new(p: &str) -> Self {
        let pat: Vec<u8> = p.bytes().map(|b| b.to_ascii_lowercase()).collect();
        let mut fail = vec![0; pat.len()];
        let mut k = 0;
        for i in 1..pat.len() {
            while k > 0 && pat[i] != pat[k] {
                k = fail[k - 1];
            }
            if pat[i] == pat[k] {
                k += 1;
            }
            fail[i] = k;
        }
        Substr { pat, fail }
    }
}

impl Automaton for Substr {
    type State = Option<usize>;
    fn start(&self) -> Option<usize> {
        if self.pat.is_empty() { None } else { Some(0) }
    }
    fn is_match(&self, s: &Option<usize>) -> bool {
        s.is_none()
    }
    fn will_always_match(&self, s: &Option<usize>) -> bool {
        s.is_none()
    }
    fn accept(&self, s: &Option<usize>, b: u8) -> Option<usize> {
        let mut k = (*s)?;
        let b = b.to_ascii_lowercase();
        while k > 0 && b != self.pat[k] {
            k = self.fail[k - 1];
        }
        if b == self.pat[k] {
            k += 1;
        }
        if k == self.pat.len() { None } else { Some(k) }
    }
}

fn encode_rows(post: &mut Vec<u8>, rows: &[(u32, u32, u8)]) {
    varint(post, rows.len() as u64);
    let (mut pf, mut pl) = (0u32, 0u32);
    for (k, &(f, l, kind)) in rows.iter().enumerate() {
        let fd = if k == 0 { f } else { f - pf };
        let ld = if k > 0 && fd == 0 { l - pl } else { l };
        assert!(kind < 32);
        varint(post, fd as u64);
        varint(post, ((ld as u64) << 5) | kind as u64);
        (pf, pl) = (f, l);
    }
}

// 定義は名前ごとに (file, line, kind) を差分 varint で詰める
fn build_names(v: &View<'_>, old2new: &[u32]) -> Res<(Vec<u8>, Vec<u8>, u64)> {
    let mut nb = MapBuilder::memory();
    let (mut post, mut uniq, mut i, n) = (Vec::new(), 0u64, 0, v.defs.len());
    while i < n {
        let name = &v.defs.get(i)[DEF_HDR..];
        let end = (i..n).find(|&j| &v.defs.get(j)[DEF_HDR..] != name).unwrap_or(n);
        let mut rows: Vec<_> = (i..end)
            .map(|j| {
                let r = v.defs.get(j);
                (old2new[u32_at(r, 0) as usize], u32_at(r, 4), r[8])
            })
            .collect();
        rows.sort_unstable();
        nb.insert(name, post.len() as u64)?;
        encode_rows(&mut post, &rows);
        uniq += 1;
        i = end;
    }
    Ok((nb.into_inner()?, post, uniq))
}

fn bench(names: &Map<Vec<u8>>, pf: raw::Fst<Vec<u8>>, paths: &[(&[u8], u32)]) -> Res<()> {
    let t = Instant::now();
    for (rank, (p, _)) in paths.iter().enumerate() {
        assert_eq!(key_by_rank(&pf, rank as u64).as_deref(), Some(*p), "rank {rank}");
    }
    println!("key_by_rank verified for all {} paths in {:?}", paths.len(), t.elapsed());
    let t = Instant::now();
    for q in ["WalkBuilder", "Marshal", "addCodeLensHandler", "nonexistent_zzz"] {
        let _ = names.get(q);
    }
    println!("4 exact lookups: {:?}", t.elapsed());
    for q in ["walkbuild", "codelenshandler"] {
        let t = Instant::now();
        let hits = names.search(Substr::new(q)).into_stream().into_str_keys()?;
        println!("substr '{q}' hits={} {:?} sample={:?}", hits.len(), t.elapsed(), hits.iter().take(3).collect::<Vec<_>>());
    }
    let pm = Map::new(pf.into_inner())?;
    let t = Instant::now();
    let hits = pm.search(Substr::new("tree-sitter-tags-0.27.2/src")).into_stream().into_str_keys()?;
    println!("path substr hits={} {:?}", hits.len(), t.elapsed());
    Ok(())
}

pub fn run(idx: &Path) -> Res<()> {
    let mm = open(idx)?;
    let v = View::parse(&mm)?;
    // パスは鍵順の rank を ID にする
    let mut paths: Vec<(&[u8], u32)> = (0..v.files.len()).map(|i| (&v.files.get(i)[FILE_HDR..], i as u32)).collect();
    paths.sort_unstable();
    let mut old2new = vec![0u32; paths.len()];
    let mut pb = MapBuilder::memory();
    for (rank, (p, old)) in paths.iter().enumerate() {
        old2new[*old as usize] = rank as u32;
        pb.insert(p, rank as u64)?;
    }
    let path_fst = pb.into_inner()?;
    let (name_fst, post, uniq) = build_names(&v, &old2new)?;
    let new_bytes = path_fst.len() + name_fst.len() + post.len();
    println!("defs={} unique_names={uniq} files={}", v.defs.len(), paths.len());
    println!(
        "old_index={} new_total={new_bytes} (paths_fst={} names_fst={} postings={}) ratio={:.3}",
        mm.len(), path_fst.len(), name_fst.len(), post.len(), new_bytes as f64 / mm.len() as f64
    );
    std::fs::write("../names.fst", &name_fst)?;
    std::fs::write("../paths.fst", &path_fst)?;
    let names = Map::new(name_fst)?;
    let raw_paths: usize = paths.iter().map(|(p, _)| p.len()).sum();
    println!("raw_path_bytes={raw_paths}");
    bench(&names, raw::Fst::new(path_fst)?, &paths)
}
