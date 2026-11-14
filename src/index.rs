use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt, fs,
    io::{self, Write},
    path::Path,
    sync::Arc,
};

use fst::{Map, MapBuilder, Streamer};
use memmap2::Mmap;

use crate::codec::{Corrupt, Row, decode_rows, encode_rows, key_by_rank};
use crate::extract::Def;
use crate::intern::{PackedRef, intern, names_of};
use crate::lang::extra_queries;
use crate::store::write_atomic;

pub const LANG_DELETED: u8 = 254;
pub const LANG_BINARY: u8 = 255;

const MAGIC: &[u8; 8] = b"FERRIDX\0";
/// 抽出結果の意味が変わる変更（クエリ、kind の割り当て、行列の数え方）では必ず上げる。
const FORMAT_VERSION: u32 = 7;
const SECTION_COUNT: usize = 7;
/// 区画ごとに 長さ u64 と CRC32C u32。位置は長さの総和で決まる。
const SECTION_ENTRY_LEN: usize = 12;
const HEADER_LEN: usize = 8 + 4 + 8 + SECTION_COUNT * SECTION_ENTRY_LEN + 4;
const STAMP_LEN: usize = 8 + 8 + 1;
const OFFSET_LEN: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: Vec<u8>,
    pub mtime_ns: u64,
    pub size: u64,
    /// `LANG_DELETED` と `LANG_BINARY` は予約。
    pub lang: u8,
    pub defs: Vec<Def>,
    pub refs: Vec<PackedRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: Vec<u8>,
    pub row: Row,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub mtime_ns: u64,
    pub size: u64,
    pub lang: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    pub files: usize,
    pub names: usize,
    pub bytes: usize,
}

#[derive(Debug)]
pub enum OpenError {
    Io(io::Error),
    Corrupt,
    /// 索引を作った抽出器と今の抽出器が違う。作り直す。
    Fingerprint,
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::Io(e) => write!(f, "cannot read index: {e}"),
            OpenError::Corrupt => f.write_str("index is corrupt"),
            OpenError::Fingerprint => f.write_str("index was built by a different extractor"),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<io::Error> for OpenError {
    fn from(e: io::Error) -> Self {
        OpenError::Io(e)
    }
}

impl From<Corrupt> for OpenError {
    fn from(_: Corrupt) -> Self {
        OpenError::Corrupt
    }
}

/// 文法のバージョンは Cargo.toml の tree-sitter 行から取る。更新を手で写し忘れない。
fn fingerprint() -> u64 {
    let grammars = include_str!("../Cargo.toml")
        .lines()
        .filter(|l| l.starts_with("tree-sitter"))
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!(
        "{FORMAT_VERSION}\n{}\n{grammars}\n{}",
        env!("CARGO_PKG_VERSION"),
        extra_queries()
    );
    // FNV-1a。衝突耐性は要らず、変更検知だけでよい
    fnv1a(text.as_bytes())
}

fn fnv1a(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_owned())
}

fn to_u32(n: usize) -> io::Result<u32> {
    u32::try_from(n).map_err(|_| invalid("too many entries"))
}

fn build_fst<'a>(pairs: impl Iterator<Item = (&'a [u8], u64)>) -> io::Result<Vec<u8>> {
    let mut b = MapBuilder::memory();
    for (k, v) in pairs {
        b.insert(k, v).map_err(io::Error::other)?;
    }
    b.into_inner().map_err(io::Error::other)
}

/// 経路 FST の値と名前 FST の値は、鍵順の順位そのもの（`key_by_rank` の前提）。
fn sections(files: &[FileEntry]) -> io::Result<[Vec<u8>; SECTION_COUNT]> {
    let mut order: Vec<&FileEntry> = files.iter().collect();
    order.sort_by(|a, b| a.path.cmp(&b.path));
    if order.windows(2).any(|w| w[0].path == w[1].path) {
        return Err(invalid("duplicate path"));
    }
    to_u32(order.len())?;

    let used: Vec<u32> = order
        .iter()
        .flat_map(|f| &f.refs)
        .flat_map(|r| [Some(r.name), r.container.checked_sub(1)])
        .flatten()
        .collect::<BTreeSet<u32>>()
        .into_iter()
        .collect();
    let ref_names = names_of(&used);
    let mut by_name: BTreeMap<&[u8], Vec<Row>> = BTreeMap::new();
    for d in order.iter().flat_map(|f| &f.defs) {
        by_name.entry(d.name.as_bytes()).or_default();
        if let Some(c) = &d.container {
            by_name.entry(c.as_bytes()).or_default();
        }
    }
    for n in &ref_names {
        by_name.entry(n.as_bytes()).or_default();
    }
    let names: Vec<&[u8]> = by_name.keys().copied().collect();
    to_u32(names.len())?;
    let rank_of = |n: &[u8]| {
        // 直前の走査で全ての名前と包含元を入れてあるので必ず見つかる
        names.binary_search(&n).expect("name was inserted") as u32
    };
    let mut rank_of_id = vec![0u32; used.last().map_or(0, |&m| m as usize + 1)];
    for (&id, n) in used.iter().zip(&ref_names) {
        rank_of_id[id as usize] = rank_of(n.as_bytes());
    }
    let mut by_ref: Vec<Vec<Row>> = vec![Vec::new(); names.len()];
    for (id, f) in order.iter().enumerate() {
        let file = id as u32;
        for d in &f.defs {
            let row = Row {
                file,
                line: d.line,
                col: d.col,
                kind: d.kind,
                container: d
                    .container
                    .as_ref()
                    .map_or(0, |c| rank_of(c.as_bytes()) + 1),
            };
            by_name.entry(d.name.as_bytes()).or_default().push(row);
        }
        for r in &f.refs {
            by_ref[rank_of_id[r.name as usize] as usize].push(Row {
                file,
                line: r.line,
                col: r.col,
                kind: r.kind,
                container: r
                    .container
                    .checked_sub(1)
                    .map_or(0, |c| rank_of_id[c as usize] + 1),
            });
        }
    }

    let (offsets, postings) = posting_sections(by_name.values_mut());
    let (ref_offsets, ref_postings) = posting_sections(by_ref.iter_mut());

    let stamps = order
        .iter()
        .flat_map(|f| {
            let mut s = [0u8; STAMP_LEN];
            s[..8].copy_from_slice(&f.mtime_ns.to_le_bytes());
            s[8..16].copy_from_slice(&f.size.to_le_bytes());
            s[16] = f.lang;
            s
        })
        .collect();
    let paths = build_fst(
        order
            .iter()
            .zip(0u64..)
            .map(|(f, i)| (f.path.as_slice(), i)),
    )?;
    let names = build_fst(names.iter().copied().zip(0u64..))?;
    Ok([
        paths,
        names,
        offsets,
        postings,
        stamps,
        ref_offsets,
        ref_postings,
    ])
}

/// 名前の順位順に並べた行の列から、オフセット表と posting を作る。
fn posting_sections<'a>(lists: impl Iterator<Item = &'a mut Vec<Row>>) -> (Vec<u8>, Vec<u8>) {
    let (mut offsets, mut postings) = (Vec::new(), Vec::new());
    for rows in lists {
        rows.sort_by_key(|r| (r.file, r.line, r.col));
        offsets.extend_from_slice(&(postings.len() as u64).to_le_bytes());
        encode_rows(&mut postings, rows);
    }
    offsets.extend_from_slice(&(postings.len() as u64).to_le_bytes());
    (offsets, postings)
}

fn assemble(fp: u64, version: u32, s: &[Vec<u8>; SECTION_COUNT]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + s.iter().map(Vec::len).sum::<usize>());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&fp.to_le_bytes());
    for sec in s {
        out.extend_from_slice(&(sec.len() as u64).to_le_bytes());
        out.extend_from_slice(&crc32c::crc32c(sec).to_le_bytes());
    }
    out.extend_from_slice(&crc32c::crc32c(&out).to_le_bytes());
    for sec in s {
        out.extend_from_slice(sec);
    }
    out
}

pub fn contains_ignore_ascii_case(hay: &[u8], needle: &[u8]) -> bool {
    // windows(0) は panic する
    needle.is_empty()
        || hay
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle))
}

pub fn write_index(path: &Path, files: &[FileEntry]) -> io::Result<()> {
    write_with(path, files, fingerprint())
}

fn write_with(path: &Path, files: &[FileEntry], fp: u64) -> io::Result<()> {
    let bytes = assemble(fp, FORMAT_VERSION, &sections(files)?);
    write_atomic(path, |f| f.write_all(&bytes))
}

struct Section {
    mmap: Arc<Mmap>,
    start: usize,
    end: usize,
}

impl AsRef<[u8]> for Section {
    fn as_ref(&self) -> &[u8] {
        &self.mmap[self.start..self.end]
    }
}

pub struct Index {
    paths: Map<Section>,
    names: Map<Section>,
    offsets: Section,
    postings: Section,
    stamps: Section,
    ref_offsets: Section,
    ref_postings: Section,
    files: u32,
    names_len: u32,
    bytes: usize,
}

fn le_u64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().expect("caller passes 8 bytes"))
}

fn le_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().expect("caller passes 4 bytes"))
}

fn open_fst(s: Section) -> Result<Map<Section>, Corrupt> {
    // 値が順位であることは書き手が保証する。偶発的な破損は先の CRC で弾く
    Map::new(s).map_err(|_| Corrupt)
}

impl Index {
    /// 全区画の CRC32C を、FST に触れる前に検証する。
    pub fn open(path: &Path) -> Result<Index, OpenError> {
        let f = fs::File::open(path)?;
        // 長さ 0 の mmap は失敗するので、先に破損として返す
        if f.metadata()?.len() == 0 {
            return Err(OpenError::Corrupt);
        }
        // SAFETY: 書き手は rename で差し替えるので、開いた後に中身が書き換わらない。
        let mmap = Arc::new(unsafe { Mmap::map(&f)? });
        let b: &[u8] = &mmap;
        let (head, rest) = b.split_at_checked(HEADER_LEN).ok_or(Corrupt)?;
        let (body, crc) = head.split_at(HEADER_LEN - 4);
        if crc32c::crc32c(body) != le_u32(crc) || &body[..8] != MAGIC {
            return Err(OpenError::Corrupt);
        }
        if le_u32(&body[8..]) != FORMAT_VERSION || le_u64(&body[12..]) != fingerprint() {
            return Err(OpenError::Fingerprint);
        }
        let mut sections = Vec::with_capacity(SECTION_COUNT);
        let mut start = HEADER_LEN;
        for e in body[20..].as_chunks::<SECTION_ENTRY_LEN>().0.iter() {
            let len = usize::try_from(le_u64(e)).map_err(|_| Corrupt)?;
            let end = start
                .checked_add(len)
                .filter(|&end| end <= b.len())
                .ok_or(Corrupt)?;
            if crc32c::crc32c(&rest[start - HEADER_LEN..end - HEADER_LEN]) != le_u32(&e[8..]) {
                return Err(OpenError::Corrupt);
            }
            sections.push(Section {
                mmap: Arc::clone(&mmap),
                start,
                end,
            });
            start = end;
        }
        if start != b.len() {
            return Err(OpenError::Corrupt);
        }
        let mut it = sections.into_iter();
        let mut next = || it.next().ok_or(Corrupt);
        let (paths, names, offsets, postings, stamps, ref_offsets, ref_postings) = (
            next()?,
            next()?,
            next()?,
            next()?,
            next()?,
            next()?,
            next()?,
        );

        let paths = open_fst(paths)?;
        let names = open_fst(names)?;
        let files = u32::try_from(paths.len()).map_err(|_| Corrupt)?;
        let names_len = u32::try_from(names.len()).map_err(|_| Corrupt)?;
        if stamps.as_ref().len() != paths.len() * STAMP_LEN
            || offsets.as_ref().len() != (names.len() + 1) * OFFSET_LEN
            || ref_offsets.as_ref().len() != (names.len() + 1) * OFFSET_LEN
        {
            return Err(OpenError::Corrupt);
        }
        Ok(Index {
            paths,
            names,
            offsets,
            postings,
            stamps,
            ref_offsets,
            ref_postings,
            files,
            names_len,
            bytes: b.len(),
        })
    }

    fn postings_range<'a>(
        offsets: &'a Section,
        postings: &'a Section,
        rank: usize,
    ) -> Option<&'a [u8]> {
        let at = |i: usize| {
            let o = offsets.as_ref().get(i * OFFSET_LEN..(i + 1) * OFFSET_LEN)?;
            usize::try_from(le_u64(o)).ok()
        };
        postings.as_ref().get(at(rank)?..at(rank + 1)?)
    }

    fn hits(
        &self,
        offsets: &Section,
        postings: &Section,
        name: &[u8],
    ) -> Result<Vec<Hit>, Corrupt> {
        let Some(rank) = self.names.get(name) else {
            return Ok(Vec::new());
        };
        let slice = Self::postings_range(offsets, postings, rank as usize).ok_or(Corrupt)?;
        decode_rows(slice, self.files, self.names_len)?
            .into_iter()
            .map(|row| {
                let path = key_by_rank(self.paths.as_fst(), u64::from(row.file)).ok_or(Corrupt)?;
                Ok(Hit { path, row })
            })
            .collect()
    }

    /// 破損した postings は空に見せず Corrupt で返す。
    pub fn lookup(&self, name: &[u8]) -> Result<Vec<Hit>, Corrupt> {
        self.hits(&self.offsets, &self.postings, name)
    }

    /// 参照の位置。`row.kind` は参照の種類。
    pub fn lookup_refs(&self, name: &[u8]) -> Result<Vec<Hit>, Corrupt> {
        self.hits(&self.ref_offsets, &self.ref_postings, name)
    }

    /// 包含元の名前。`row.container` は名前 FST の順位+1、0 は包含元なし。
    pub fn container_name(&self, row: &Row) -> Option<Vec<u8>> {
        key_by_rank(
            self.names.as_fst(),
            u64::from(row.container.checked_sub(1)?),
        )
    }

    /// 経路の昇順（= ファイル id の順）。
    pub fn files(&self) -> impl Iterator<Item = (Vec<u8>, Stamp)> + '_ {
        let mut keys = self.paths.stream();
        let stamps = self.stamps.as_ref().as_chunks::<STAMP_LEN>().0;
        let keys = std::iter::from_fn(move || keys.next().map(|(k, _)| k.to_vec()));
        keys.zip(stamps).map(|(k, s)| {
            let stamp = Stamp {
                mtime_ns: le_u64(s),
                size: le_u64(&s[8..]),
                lang: s[16],
            };
            (k, stamp)
        })
    }

    /// 名前に `needle` を含むもの（ASCII の大小は無視）を名前の昇順で返す。
    pub fn names_containing<'a>(&'a self, needle: &'a [u8]) -> impl Iterator<Item = Vec<u8>> + 'a {
        let mut stream = self.names.stream();
        std::iter::from_fn(move || {
            while let Some((name, _)) = stream.next() {
                if contains_ignore_ascii_case(name, needle) {
                    return Some(name.to_vec());
                }
            }
            None
        })
    }

    /// 区画ごとのバイト数（ヘッダを除く）。
    pub fn section_bytes(&self) -> [(&'static str, usize); SECTION_COUNT] {
        [
            ("paths", self.paths.as_fst().as_bytes().len()),
            ("names", self.names.as_fst().as_bytes().len()),
            ("offsets", self.offsets.as_ref().len()),
            ("postings", self.postings.as_ref().len()),
            ("stamps", self.stamps.as_ref().len()),
            ("ref_offsets", self.ref_offsets.as_ref().len()),
            ("ref_postings", self.ref_postings.as_ref().len()),
        ]
    }

    pub fn has_path(&self, path: &[u8]) -> bool {
        self.paths.contains_key(path)
    }

    /// 全ファイルを定義つきで復元する。`body` は保存していないので空範囲になる。
    pub fn entries(&self) -> Result<Vec<FileEntry>, Corrupt> {
        let mut out: Vec<FileEntry> = self
            .files()
            .map(|(path, s)| FileEntry {
                path,
                mtime_ns: s.mtime_ns,
                size: s.size,
                lang: s.lang,
                defs: Vec::new(),
                refs: Vec::new(),
            })
            .collect();
        let mut ids: HashMap<u64, u32> = HashMap::new();
        let intern_name = |ids: &mut HashMap<u64, u32>, rank: u64| -> Result<u32, Corrupt> {
            if let Some(&id) = ids.get(&rank) {
                return Ok(id);
            }
            let n = key_by_rank(self.names.as_fst(), rank).ok_or(Corrupt)?;
            let id = intern(std::str::from_utf8(&n).map_err(|_| Corrupt)?);
            ids.insert(rank, id);
            Ok(id)
        };
        let mut names = self.names.stream();
        while let Some((name, rank)) = names.next() {
            let name = String::from_utf8(name.to_vec()).map_err(|_| Corrupt)?;
            let rank = rank as usize;
            let container_of = |row: &Row| {
                self.container_name(row)
                    .map(String::from_utf8)
                    .transpose()
                    .map_err(|_| Corrupt)
            };
            let defs = Self::postings_range(&self.offsets, &self.postings, rank).ok_or(Corrupt)?;
            for row in decode_rows(defs, self.files, self.names_len)? {
                let file = out.get_mut(row.file as usize).ok_or(Corrupt)?;
                file.defs.push(Def {
                    name: name.clone(),
                    line: row.line,
                    col: row.col,
                    kind: row.kind,
                    container: container_of(&row)?,
                    body: 0..0,
                });
            }
            let refs =
                Self::postings_range(&self.ref_offsets, &self.ref_postings, rank).ok_or(Corrupt)?;
            let name_id = intern_name(&mut ids, rank as u64)?;
            for row in decode_rows(refs, self.files, self.names_len)? {
                let container = match row.container.checked_sub(1) {
                    Some(c) => intern_name(&mut ids, u64::from(c))? + 1,
                    None => 0,
                };
                let f = out.get_mut(row.file as usize).ok_or(Corrupt)?;
                f.refs.push(PackedRef {
                    name: name_id,
                    container,
                    line: row.line,
                    col: row.col,
                    kind: row.kind,
                });
            }
        }
        Ok(out)
    }

    pub fn stats(&self) -> Stats {
        Stats {
            files: self.files as usize,
            names: self.names_len as usize,
            bytes: self.bytes,
        }
    }
}

#[cfg(test)]
mod tests;
