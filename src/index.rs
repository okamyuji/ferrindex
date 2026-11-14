use std::{
    collections::BTreeMap,
    fmt, fs,
    io::{self, Write},
    path::Path,
    sync::Arc,
};

use fst::{Map, MapBuilder, Streamer};
use memmap2::Mmap;

use crate::codec::{Corrupt, Row, decode_rows, encode_rows, key_by_rank};
use crate::extract::Def;
use crate::store::write_atomic;

pub const LANG_DELETED: u8 = 254;
pub const LANG_BINARY: u8 = 255;

const MAGIC: &[u8; 8] = b"FERRIDX\0";
/// 抽出結果の意味が変わる変更（クエリ、kind の割り当て、行列の数え方）では必ず上げる。
const FORMAT_VERSION: u32 = 1;
const SECTION_COUNT: usize = 5;
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
        "{FORMAT_VERSION}\n{}\n{grammars}",
        env!("CARGO_PKG_VERSION")
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

    let mut by_name: BTreeMap<&[u8], Vec<Row>> = BTreeMap::new();
    for d in order.iter().flat_map(|f| &f.defs) {
        by_name.entry(d.name.as_bytes()).or_default();
        if let Some(c) = &d.container {
            by_name.entry(c.as_bytes()).or_default();
        }
    }
    let names: Vec<&[u8]> = by_name.keys().copied().collect();
    to_u32(names.len())?;
    for (id, f) in order.iter().enumerate() {
        for d in &f.defs {
            let container = d.container.as_ref().map_or(0, |c| {
                // 直前の走査で全ての包含元を入れてあるので必ず見つかる
                names
                    .binary_search(&c.as_bytes())
                    .expect("container was inserted") as u32
                    + 1
            });
            let row = Row {
                file: id as u32,
                line: d.line,
                col: d.col,
                kind: d.kind,
                container,
            };
            by_name.entry(d.name.as_bytes()).or_default().push(row);
        }
    }

    let (mut offsets, mut postings) = (Vec::new(), Vec::new());
    for rows in by_name.values_mut() {
        rows.sort_by_key(|r| (r.file, r.line, r.col));
        offsets.extend_from_slice(&(postings.len() as u64).to_le_bytes());
        encode_rows(&mut postings, rows);
    }
    offsets.extend_from_slice(&(postings.len() as u64).to_le_bytes());

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
    Ok([paths, names, offsets, postings, stamps])
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
        let (paths, names, offsets, postings, stamps) =
            (next()?, next()?, next()?, next()?, next()?);

        let paths = open_fst(paths)?;
        let names = open_fst(names)?;
        let files = u32::try_from(paths.len()).map_err(|_| Corrupt)?;
        let names_len = u32::try_from(names.len()).map_err(|_| Corrupt)?;
        if stamps.as_ref().len() != paths.len() * STAMP_LEN
            || offsets.as_ref().len() != (names.len() + 1) * OFFSET_LEN
        {
            return Err(OpenError::Corrupt);
        }
        Ok(Index {
            paths,
            names,
            offsets,
            postings,
            stamps,
            files,
            names_len,
            bytes: b.len(),
        })
    }

    fn postings_range(&self, rank: usize) -> Option<&[u8]> {
        let at = |i: usize| {
            let o = self
                .offsets
                .as_ref()
                .get(i * OFFSET_LEN..(i + 1) * OFFSET_LEN)?;
            usize::try_from(le_u64(o)).ok()
        };
        self.postings.as_ref().get(at(rank)?..at(rank + 1)?)
    }

    /// 破損した postings は空に見せず Corrupt で返す。
    pub fn lookup(&self, name: &[u8]) -> Result<Vec<Hit>, Corrupt> {
        let Some(rank) = self.names.get(name) else {
            return Ok(Vec::new());
        };
        let slice = self.postings_range(rank as usize).ok_or(Corrupt)?;
        decode_rows(slice, self.files, self.names_len)?
            .into_iter()
            .map(|row| {
                let path = key_by_rank(self.paths.as_fst(), u64::from(row.file)).ok_or(Corrupt)?;
                Ok(Hit { path, row })
            })
            .collect()
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

    pub fn stats(&self) -> Stats {
        Stats {
            files: self.files as usize,
            names: self.names_len as usize,
            bytes: self.bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn def(name: &str, line: u32, container: Option<&str>) -> Def {
        Def {
            name: name.into(),
            line,
            col: 3,
            kind: 2,
            container: container.map(Into::into),
            body: 0..0,
        }
    }

    fn entry(path: &str, defs: Vec<Def>) -> FileEntry {
        FileEntry {
            path: path.into(),
            mtime_ns: 1_700_000_000_123_456_789,
            size: 4242,
            lang: 7,
            defs,
        }
    }

    fn sample() -> Vec<FileEntry> {
        vec![
            entry(
                "src/b.rs",
                vec![def("Alpha", 9, None), def("run", 12, Some("Alpha"))],
            ),
            entry(
                "src/a.rs",
                vec![def("Alpha", 1, None), def("zeta", 5, None)],
            ),
            FileEntry {
                lang: LANG_DELETED,
                ..entry("old.rs", vec![])
            },
        ]
    }

    fn written(files: &[FileEntry]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("index.fx");
        write_index(&p, files).unwrap();
        (dir, p)
    }

    fn open_bytes(bytes: &[u8]) -> Result<Index, OpenError> {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.fx");
        fs::write(&p, bytes).unwrap();
        Index::open(&p)
    }

    fn is_corrupt<T>(r: Result<T, OpenError>) -> bool {
        matches!(r, Err(OpenError::Corrupt))
    }

    #[test]
    fn write_then_lookup() {
        let (_d, p) = written(&sample());
        let ix = Index::open(&p).unwrap();
        let hits = ix.lookup(b"Alpha").unwrap();
        let got: Vec<(&[u8], u32, u32)> = hits
            .iter()
            .map(|h| (h.path.as_slice(), h.row.line, h.row.col))
            .collect();
        assert_eq!(got, [(&b"src/a.rs"[..], 1, 3), (&b"src/b.rs"[..], 9, 3)]);
        assert_eq!(hits[0].row.kind, 2);
        assert_eq!(hits[0].row.container, 0);
        let run = ix.lookup(b"run").unwrap();
        assert_eq!(run.len(), 1);
        assert_eq!(
            ix.container_name(&run[0].row).as_deref(),
            Some(&b"Alpha"[..])
        );
        assert_eq!(ix.container_name(&hits[0].row), None);
        assert!(ix.lookup(b"missing").unwrap().is_empty());
        assert!(ix.lookup(b"Alph").unwrap().is_empty());
    }

    #[test]
    fn container_without_own_rows_is_resolvable() {
        let files = [entry("a.rs", vec![def("m", 2, Some("OnlyContainer"))])];
        let (_d, p) = written(&files);
        let ix = Index::open(&p).unwrap();
        let h = &ix.lookup(b"m").unwrap()[0];
        assert_eq!(
            ix.container_name(&h.row).as_deref(),
            Some(&b"OnlyContainer"[..])
        );
        assert!(ix.lookup(b"OnlyContainer").unwrap().is_empty());
    }

    #[test]
    fn file_id_is_byte_order_rank() {
        let (_d, p) = written(&sample());
        let ix = Index::open(&p).unwrap();
        let paths: Vec<Vec<u8>> = ix.files().map(|(k, _)| k).collect();
        assert_eq!(paths, [&b"old.rs"[..], b"src/a.rs", b"src/b.rs"]);
        let ids: Vec<u32> = ix
            .lookup(b"Alpha")
            .unwrap()
            .iter()
            .map(|h| h.row.file)
            .collect();
        assert_eq!(ids, [1, 2]);
        let stamps: Vec<Stamp> = ix.files().map(|(_, s)| s).collect();
        assert_eq!(stamps[0].lang, LANG_DELETED);
        assert_eq!(
            stamps[2],
            Stamp {
                mtime_ns: 1_700_000_000_123_456_789,
                size: 4242,
                lang: 7
            }
        );
    }

    #[test]
    fn stats_count_files_names_and_bytes() {
        let (_d, p) = written(&sample());
        let ix = Index::open(&p).unwrap();
        let s = ix.stats();
        assert_eq!((s.files, s.names), (3, 3));
        assert_eq!(s.bytes as u64, fs::metadata(&p).unwrap().len());
    }

    #[test]
    fn duplicate_paths_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let files = [entry("a", vec![]), entry("a", vec![])];
        let e = write_index(&dir.path().join("i.fx"), &files).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn rewrite_replaces_and_leaves_no_temp_file() {
        let (d, p) = written(&sample());
        write_index(&p, &[entry("only.rs", vec![def("solo", 1, None)])]).unwrap();
        let ix = Index::open(&p).unwrap();
        assert_eq!(ix.lookup(b"solo").unwrap().len(), 1);
        assert!(ix.lookup(b"Alpha").unwrap().is_empty());
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn format_constants_and_helpers() {
        assert_eq!((HEADER_LEN, STAMP_LEN), (84, 17));
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert!(fingerprint() > 1);
        assert_eq!(to_u32(7).unwrap(), 7);
        assert_eq!(
            to_u32(u32::MAX as usize + 1).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(OpenError::Corrupt.to_string(), "index is corrupt");
        assert_eq!(
            OpenError::Fingerprint.to_string(),
            "index was built by a different extractor"
        );
        let io = OpenError::Io(io::Error::other("boom")).to_string();
        assert_eq!(io, "cannot read index: boom");
    }

    #[test]
    fn missing_file_is_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let r = Index::open(&dir.path().join("none.fx"));
        assert!(matches!(r, Err(OpenError::Io(e)) if e.kind() == io::ErrorKind::NotFound));
    }

    #[test]
    fn corrupt_section_detected() {
        let (_d, p) = written(&sample());
        let good = fs::read(&p).unwrap();
        let mut start = HEADER_LEN;
        for i in 0..5 {
            let len =
                u64::from_le_bytes(good[20 + i * 12..28 + i * 12].try_into().unwrap()) as usize;
            assert!(len > 0, "section {i} is empty");
            let mut flipped = good.clone();
            flipped[start] ^= 1;
            assert!(is_corrupt(open_bytes(&flipped)), "bit flip in section {i}");
            let mut zeroed = good.clone();
            zeroed[start..start + len].fill(0);
            assert!(is_corrupt(open_bytes(&zeroed)), "zero fill in section {i}");
            start += len;
        }
        assert_eq!(start, good.len());
    }

    #[test]
    fn every_single_bit_flip_is_detected() {
        let (_d, p) = written(&sample());
        let good = fs::read(&p).unwrap();
        for i in 0..good.len() {
            let mut b = good.clone();
            b[i] ^= 0x80;
            assert!(is_corrupt(open_bytes(&b)), "byte {i}");
        }
    }

    #[test]
    fn truncation_and_trailing_bytes_are_detected() {
        let (_d, p) = written(&sample());
        let good = fs::read(&p).unwrap();
        for n in [0, 1, HEADER_LEN - 1, HEADER_LEN, good.len() - 1] {
            assert!(is_corrupt(open_bytes(&good[..n])), "len {n}");
        }
        let mut longer = good.clone();
        longer.push(0);
        assert!(is_corrupt(open_bytes(&longer)));
    }

    fn assembled(fp: u64, version: u32, s: &[Vec<u8>; 5]) -> Result<Index, OpenError> {
        open_bytes(&assemble(fp, version, s))
    }

    #[test]
    fn fingerprint_mismatch_detected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("i.fx");
        write_with(&p, &sample(), fingerprint() ^ 1).unwrap();
        assert!(matches!(Index::open(&p), Err(OpenError::Fingerprint)));
        let s = sections(&sample()).unwrap();
        assert!(matches!(
            assembled(fingerprint(), FORMAT_VERSION + 1, &s),
            Err(OpenError::Fingerprint)
        ));
        assert!(assembled(fingerprint(), FORMAT_VERSION, &s).is_ok());
    }

    #[test]
    fn bad_magic_with_valid_crc_is_corrupt() {
        let s = sections(&sample()).unwrap();
        let mut b = assemble(fingerprint(), FORMAT_VERSION, &s);
        b[0] ^= 1;
        let crc = crc32c::crc32c(&b[..HEADER_LEN - 4]);
        b[HEADER_LEN - 4..HEADER_LEN].copy_from_slice(&crc.to_le_bytes());
        assert!(is_corrupt(open_bytes(&b)));
    }

    #[test]
    fn inconsistent_sections_are_corrupt() {
        let good = sections(&[entry("a", vec![def("x", 1, None)]), entry("b", vec![])]).unwrap();
        assert!(assembled(fingerprint(), FORMAT_VERSION, &good).is_ok());

        let mut s = good.clone();
        s[4].extend_from_slice(&[0; 17]);
        assert!(
            is_corrupt(assembled(fingerprint(), FORMAT_VERSION, &s)),
            "stamps too long"
        );
        let mut s = good.clone();
        s[4].pop();
        assert!(
            is_corrupt(assembled(fingerprint(), FORMAT_VERSION, &s)),
            "stamps too short"
        );
        let mut s = good.clone();
        s[2].extend_from_slice(&[0; 8]);
        assert!(
            is_corrupt(assembled(fingerprint(), FORMAT_VERSION, &s)),
            "offsets too long"
        );
        let mut s = good.clone();
        s[2].truncate(8);
        assert!(
            is_corrupt(assembled(fingerprint(), FORMAT_VERSION, &s)),
            "offsets too short"
        );
        let mut s = good.clone();
        s[0] = vec![1, 2, 3];
        assert!(
            is_corrupt(assembled(fingerprint(), FORMAT_VERSION, &s)),
            "path fst garbage"
        );
        let mut s = good.clone();
        s[1] = vec![1, 2, 3];
        assert!(
            is_corrupt(assembled(fingerprint(), FORMAT_VERSION, &s)),
            "name fst garbage"
        );
    }

    #[test]
    fn lookup_reports_corrupt_postings() {
        let good = sections(&[entry("a", vec![def("x", 1, None)])]).unwrap();
        let end = u64::try_from(good[3].len()).unwrap();
        let lookup = |s: &[Vec<u8>; 5]| {
            assembled(fingerprint(), FORMAT_VERSION, s)
                .unwrap()
                .lookup(b"x")
        };
        assert_eq!(lookup(&good).unwrap().len(), 1);

        let mut s = good.clone();
        s[2] = [0u64, end + 1]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert!(lookup(&s).is_err(), "end past postings");
        let mut s = good.clone();
        s[2] = [end, 0u64].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert!(lookup(&s).is_err(), "start after end");
        let mut s = good.clone();
        s[3] = vec![0xff; good[3].len()];
        assert!(lookup(&s).is_err(), "garbage postings");
        let mut s = good.clone();
        s[3] = vec![1, 5, 0, 0, 0];
        s[2] = [0u64, 5].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert!(lookup(&s).is_err(), "file id out of range");
        let mut s = good.clone();
        s[3] = vec![1, 0, 0, 0, 9];
        s[2] = [0u64, 5].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert!(lookup(&s).is_err(), "container out of range");
    }
}
