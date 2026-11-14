//! 別プロセスが書き続ける中で、2つの検索を並列に走らせる。
//! 各検索は「目印を書く直前までに閉じ終えたファイル」が、それまでの結果の和に入っているかを確かめる。
//! 使い方: race <work_dir> <sentinel_dir> <seconds> <shared_sentinel 0|1>
use fsevents_probe::{current_id, query, DROPPED};
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

fn writer(dir: String, log: String, secs: u64, pace: bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    let mut k = 0u64;
    let mut lf = std::fs::OpenOptions::new().create(true).append(true).open(&log).unwrap();
    while Instant::now() < end {
        let p = format!("{dir}/w{k}.rs");
        std::fs::write(&p, format!("fn w{k}(){{}}")).unwrap();
        if pace { std::thread::sleep(Duration::from_millis(1)); }
        // 閉じ終えた後に記録する
        writeln!(lf, "{p}").unwrap();
        lf.flush().unwrap();
        k += 1;
    }
}

fn closed_so_far(log: &str) -> Vec<String> {
    let f = std::fs::File::open(log).unwrap();
    std::io::BufReader::new(f).lines().map_while(Result::ok).collect()
}

fn searcher(no: u32, dir: String, sdir: String, log: String, secs: u64, shared: bool) -> (u64, u64, u64) {
    let end = Instant::now() + Duration::from_secs(secs);
    let mut since = current_id();
    let mut seen: HashSet<String> = HashSet::new();
    let (mut queries, mut misses, mut timeouts) = (0, 0, 0);
    let (mut dropped, mut rescan) = (0u64, Vec::<String>::new());
    let mut i = 0u64;
    while Instant::now() < end {
        let before = closed_so_far(&log);
        let name = if shared { format!("{sdir}/sentinel") } else { format!("{sdir}/s{no}-{i}") };
        let q = query(&[&dir, &sdir], &name, since, Duration::from_secs(5));
        i += 1;
        queries += 1;
        let Some(sid) = q.sentinel_id else { timeouts += 1; continue };
        seen.extend(q.events.iter().map(|e| e.path.clone()));
        // 捨てられたイベントは「その下を走査し直す」印として扱う
        for e in q.events.iter().filter(|e| e.flags & DROPPED != 0) {
            dropped += 1;
            rescan.push(e.path.trim_end_matches('/').to_string());
        }
        let covered = |p: &String| seen.contains(p) || rescan.iter().any(|d| p.starts_with(&format!("{d}/")));
        misses += before.iter().filter(|p| !covered(p)).count() as u64;
        since = sid;
    }
    eprintln!("searcher={no} dropped_events={dropped}");
    (queries, misses, timeouts)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (dir, sdir, secs, shared) = (a[1].clone(), a[2].clone(), a[3].parse().unwrap(), a[4] == "1");
    let pace = a.get(5).is_some_and(|v| v == "pace");
    let log = format!("{sdir}/closed.log");
    let _ = std::fs::remove_file(&log);
    std::fs::File::create(&log).unwrap();
    let w = { let (d, l) = (dir.clone(), log.clone()); std::thread::spawn(move || writer(d, l, secs, pace)) };
    let hs: Vec<_> = (0..2)
        .map(|no| {
            let (d, s, l) = (dir.clone(), sdir.clone(), log.clone());
            std::thread::spawn(move || searcher(no, d, s, l, secs, shared))
        })
        .collect();
    w.join().unwrap();
    for (no, h) in hs.into_iter().enumerate() {
        let (q, m, t) = h.join().unwrap();
        println!("searcher={no} queries={q} misses={m} timeouts={t}");
    }
}
