use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corrupt;

impl fmt::Display for Corrupt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("corrupt index data")
    }
}

impl std::error::Error for Corrupt {}

pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub fn get_varint(b: &mut &[u8]) -> Result<u64, Corrupt> {
    let mut v = 0u64;
    for shift in (0..70).step_by(7) {
        let (&byte, rest) = b.split_first().ok_or(Corrupt)?;
        *b = rest;
        let bits = u64::from(byte & 0x7f);
        // 10 バイト目は最上位 1 ビットしか入らない
        if shift == 63 && bits > 1 {
            return Err(Corrupt);
        }
        v |= bits << shift;
        if byte < 0x80 {
            return Ok(v);
        }
    }
    Err(Corrupt)
}

/// `container` は名前 FST の順位+1。0 は包含元なし。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub file: u32,
    pub line: u32,
    pub col: u32,
    pub kind: u8,
    pub container: u32,
}

const KIND_BITS: u32 = 4;

/// `rows` は (file, line, col) の昇順。同じファイルでは line を差分にする。
pub fn encode_rows(out: &mut Vec<u8>, rows: &[Row]) {
    put_varint(out, rows.len() as u64);
    let mut prev: Option<&Row> = None;
    for r in rows {
        assert!(r.kind < 1 << KIND_BITS, "kind {} does not fit", r.kind);
        let (fd, ld) = match prev {
            None => (r.file, r.line),
            Some(p) => {
                assert!((p.file, p.line) <= (r.file, r.line), "rows must be sorted");
                (
                    r.file - p.file,
                    if r.file == p.file {
                        r.line - p.line
                    } else {
                        r.line
                    },
                )
            }
        };
        put_varint(out, u64::from(fd));
        put_varint(out, (u64::from(ld) << KIND_BITS) + u64::from(r.kind));
        put_varint(out, u64::from(r.col));
        put_varint(out, u64::from(r.container));
        prev = Some(r);
    }
}

/// `max_file` はファイル数、`max_name` は名前数。範囲外の id は Corrupt。
pub fn decode_rows(mut b: &[u8], max_file: u32, max_name: u32) -> Result<Vec<Row>, Corrupt> {
    let b = &mut b;
    let n = get_varint(b)?;
    // 1 行は最低 4 バイト。n を信じて確保しない
    let mut rows = Vec::with_capacity((n as usize).min(b.len() / 4));
    let (mut file, mut line) = (0u32, 0u32);
    for _ in 0..n {
        let fd = u32::try_from(get_varint(b)?).map_err(|_| Corrupt)?;
        let packed = get_varint(b)?;
        let ld = u32::try_from(packed >> KIND_BITS).map_err(|_| Corrupt)?;
        let kind = (packed & ((1 << KIND_BITS) - 1)) as u8;
        let col = u32::try_from(get_varint(b)?).map_err(|_| Corrupt)?;
        let container = u32::try_from(get_varint(b)?).map_err(|_| Corrupt)?;
        file = file.checked_add(fd).ok_or(Corrupt)?;
        line = if fd == 0 {
            line.checked_add(ld).ok_or(Corrupt)?
        } else {
            ld
        };
        if file >= max_file || container > max_name {
            return Err(Corrupt);
        }
        rows.push(Row {
            file,
            line,
            col,
            kind,
            container,
        });
    }
    Ok(rows)
}

/// 出力（rank）が鍵順に単調増加する FST だけで成り立つ逆引き（Lucene Util.getByOutput と同じ考え方）
pub fn key_by_rank(f: &fst::raw::Fst<impl AsRef<[u8]>>, rank: u64) -> Option<Vec<u8>> {
    let (mut node, mut acc, mut key) = (f.root(), 0u64, Vec::new());
    loop {
        if node.is_final() && acc + node.final_output().value() == rank {
            return Some(key);
        }
        let t = node
            .transitions()
            .take_while(|t| acc + t.out.value() <= rank)
            .last()?;
        acc += t.out.value();
        key.push(t.inp);
        node = f.node(t.addr);
    }
}

#[cfg(test)]
mod tests {
    use fst::{MapBuilder, raw::Fst};

    use super::*;

    fn row(file: u32, line: u32, col: u32, kind: u8, container: u32) -> Row {
        Row {
            file,
            line,
            col,
            kind,
            container,
        }
    }

    #[test]
    fn varint_roundtrip() {
        for v in [0, 1, 127, 128, 16383, 16384, u64::MAX] {
            let mut buf = Vec::new();
            put_varint(&mut buf, v);
            let mut s = buf.as_slice();
            assert_eq!(get_varint(&mut s), Ok(v));
            assert!(s.is_empty());
        }
    }

    #[test]
    fn truncated_varint_is_corrupt() {
        assert_eq!(get_varint(&mut &[0x80u8][..]), Err(Corrupt));
        assert_eq!(get_varint(&mut &[][..]), Err(Corrupt));
        // 11 バイト続く / 64 ビットを超える
        assert_eq!(get_varint(&mut &[0xffu8; 11][..]), Err(Corrupt));
        let over = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02];
        assert_eq!(get_varint(&mut &over[..]), Err(Corrupt));
    }

    #[test]
    fn rows_roundtrip() {
        let rows = vec![
            row(0, 1, 1, 0, 0),
            row(0, 1, 9, 3, 2),
            row(0, 40, 5, 11, 0),
            row(3, 2, 1, 15, 7),
            row(3, 2, 30, 1, 1),
            row(9, 100000, 77, 4, 0),
        ];
        let mut buf = Vec::new();
        encode_rows(&mut buf, &rows);
        assert_eq!(decode_rows(&buf, 10, 7), Ok(rows));
        let mut empty = Vec::new();
        encode_rows(&mut empty, &[]);
        assert_eq!(decode_rows(&empty, 0, 0), Ok(vec![]));
    }

    #[test]
    fn rows_out_of_range_is_corrupt() {
        let mut buf = Vec::new();
        encode_rows(&mut buf, &[row(5, 1, 1, 0, 3)]);
        assert!(decode_rows(&buf, 6, 3).is_ok());
        assert_eq!(decode_rows(&buf, 5, 3), Err(Corrupt));
        assert_eq!(decode_rows(&buf, 6, 2), Err(Corrupt));
        // 行数が実データより多い / 欠けている
        assert_eq!(decode_rows(&buf[..buf.len() - 1], 6, 3), Err(Corrupt));
        assert_eq!(decode_rows(&[0xff, 0xff, 0xff, 0x7f], 6, 3), Err(Corrupt));
    }

    #[test]
    fn decode_never_panics_on_random_bytes() {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for _ in 0..20000 {
            let len = (next() % 40) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let _ = decode_rows(&bytes, 1000, 1000);
        }
    }

    fn fst_of(keys: &[&[u8]]) -> Fst<Vec<u8>> {
        let mut b = MapBuilder::memory();
        for (i, k) in keys.iter().enumerate() {
            b.insert(k, i as u64).unwrap();
        }
        Fst::new(b.into_inner().unwrap()).unwrap()
    }

    #[test]
    fn key_by_rank_prefix_keys() {
        let keys: [&[u8]; 5] = [b"a", b"ab", b"abc", b"src/foo.rs", b"src/foo/mod.rs"];
        let f = fst_of(&keys);
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(key_by_rank(&f, i as u64).as_deref(), Some(*k));
        }
    }

    #[test]
    fn key_by_rank_out_of_range_is_none() {
        let f = fst_of(&[b"a", b"ab"]);
        assert_eq!(key_by_rank(&f, 2), None);
        assert_eq!(key_by_rank(&f, u64::MAX / 2), None);
    }

    #[test]
    fn corrupt_displays_message() {
        assert_eq!(Corrupt.to_string(), "corrupt index data");
    }

    #[test]
    fn rows_pack_line_and_kind() {
        let mut buf = Vec::new();
        encode_rows(&mut buf, &[row(0, 3, 1, 5, 0)]);
        // count, file, line<<4|kind, col, container
        assert_eq!(buf, [1, 0, 3 << 4 | 5, 1, 0]);
    }

    #[test]
    fn rows_bounds_are_exclusive_file_inclusive_container() {
        let mut buf = Vec::new();
        encode_rows(&mut buf, &[row(2, 1, 1, 0, 4)]);
        assert!(decode_rows(&buf, 3, 4).is_ok());
        assert_eq!(decode_rows(&buf, 3, 3), Err(Corrupt));
        assert_eq!(decode_rows(&buf, 2, 4), Err(Corrupt));
    }

    #[test]
    fn key_by_rank_with_nonzero_acc_at_final() {
        // 共有接頭辞 "ab" の先で final 判定に acc が効く
        let keys: [&[u8]; 4] = [b"ab", b"abc", b"abd", b"b"];
        let f = fst_of(&keys);
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(key_by_rank(&f, i as u64).as_deref(), Some(*k));
        }
        assert_eq!(key_by_rank(&f, 4), None);
    }

    #[test]
    fn key_by_rank_adds_final_output() {
        // "ab" の値が小さいと遷移 'a' の出力は 5 になり、"a" の final 出力は 2 になる
        let mut b = MapBuilder::memory();
        for (k, v) in [(&b"a"[..], 7u64), (b"ab", 5)] {
            b.insert(k, v).unwrap();
        }
        let f = Fst::new(b.into_inner().unwrap()).unwrap();
        assert_eq!(key_by_rank(&f, 7).as_deref(), Some(&b"a"[..]));
    }
}
