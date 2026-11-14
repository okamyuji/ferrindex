//! FSEvents の履歴が「書き込みの直後」の変更を返すかを測る使い捨ての計測器。
//! 使い方: `fsevents-probe now` で現在のイベントIDを出す。
//!         `fsevents-probe since <dir> <id>` で id より後に <dir> の下で変わったパスを出す。
use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
use core_foundation::string::CFString;
use fsevent_sys as fs;
use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
use std::time::{Duration, Instant};

struct Acc {
    paths: Vec<String>,
    history_done: bool,
    must_scan: bool,
}

extern "C" fn cb(
    _s: fs::FSEventStreamRef,
    info: *mut c_void,
    n: usize,
    paths: *mut c_void,
    flags: *const fs::FSEventStreamEventFlags,
    _ids: *const fs::FSEventStreamEventId,
) {
    let acc = unsafe { &mut *(info as *mut Acc) };
    let paths = paths as *const *const c_char;
    for i in 0..n {
        let f = unsafe { *flags.add(i) };
        if f & fs::kFSEventStreamEventFlagHistoryDone != 0 {
            acc.history_done = true;
            continue;
        }
        let drop_mask = fs::kFSEventStreamEventFlagMustScanSubDirs
            | fs::kFSEventStreamEventFlagUserDropped
            | fs::kFSEventStreamEventFlagKernelDropped;
        if f & drop_mask != 0 {
            acc.must_scan = true;
        }
        let p = unsafe { CStr::from_ptr(*paths.add(i)) };
        acc.paths.push(p.to_string_lossy().into_owned());
    }
}

fn since(dir: &str, id: u64) -> (Acc, Duration) {
    let t = Instant::now();
    let mut acc = Acc { paths: vec![], history_done: false, must_scan: false };
    let ctx = fs::FSEventStreamContext {
        version: 0,
        info: &mut acc as *mut Acc as *mut c_void,
        retain: None,
        release: None,
        copy_description: None,
    };
    let arr = CFArray::from_CFTypes(&[CFString::new(dir)]);
    let flags = fs::kFSEventStreamCreateFlagFileEvents | fs::kFSEventStreamCreateFlagNoDefer;
    unsafe {
        let s = fs::FSEventStreamCreate(
            std::ptr::null_mut(), cb, &ctx, arr.as_concrete_TypeRef() as _, id, 0.0, flags,
        );
        let rl = CFRunLoop::get_current();
        fs::FSEventStreamScheduleWithRunLoop(s, rl.as_concrete_TypeRef() as _, kCFRunLoopDefaultMode as _);
        fs::FSEventStreamStart(s);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !acc.history_done && Instant::now() < deadline {
            CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, Duration::from_millis(5), true);
        }
        // 履歴の後にまだ配られていない分を同期で配らせる
        fs::FSEventStreamFlushSync(s);
        fs::FSEventStreamStop(s);
        fs::FSEventStreamInvalidate(s);
        fs::FSEventStreamRelease(s);
    }
    (acc, t.elapsed())
}

/// dirs を見張り、開始後に sentinel を書き、その到着まで待つ。id0 より後の変更を返す。
fn since_barrier(dirs: &[&str], sentinel: &str, id0: u64) -> (Acc, Duration, bool) {
    let t = Instant::now();
    let mut acc = Acc { paths: vec![], history_done: false, must_scan: false };
    let ctx = fs::FSEventStreamContext {
        version: 0,
        info: &mut acc as *mut Acc as *mut c_void,
        retain: None,
        release: None,
        copy_description: None,
    };
    let cf: Vec<CFString> = dirs.iter().map(|d| CFString::new(d)).collect();
    let arr = CFArray::from_CFTypes(&cf);
    let flags = fs::kFSEventStreamCreateFlagFileEvents | fs::kFSEventStreamCreateFlagNoDefer;
    let mut seen = false;
    unsafe {
        let s = fs::FSEventStreamCreate(
            std::ptr::null_mut(), cb, &ctx, arr.as_concrete_TypeRef() as _, id0, 0.0, flags,
        );
        let rl = CFRunLoop::get_current();
        fs::FSEventStreamScheduleWithRunLoop(s, rl.as_concrete_TypeRef() as _, kCFRunLoopDefaultMode as _);
        fs::FSEventStreamStart(s);
        std::fs::write(sentinel, t.elapsed().as_nanos().to_string()).expect("sentinel");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, Duration::from_millis(1), true);
            if acc.paths.iter().any(|p| p == sentinel) { seen = true; break; }
        }
        fs::FSEventStreamStop(s);
        fs::FSEventStreamInvalidate(s);
        fs::FSEventStreamRelease(s);
    }
    (acc, t.elapsed(), seen)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("now") => println!("{}", unsafe { fs::FSEventsGetCurrentEventId() }),
        Some("since") => {
            let id: u64 = a[3].parse().expect("id");
            let (acc, el) = since(&a[2], id);
            println!(
                "elapsed_us={} history_done={} must_scan={} n={}",
                el.as_micros(), acc.history_done, acc.must_scan, acc.paths.len()
            );
            for p in acc.paths {
                println!("{p}");
            }
        }
        Some("delay") => {
            // id を取り、ファイルを書き、指定ミリ秒待ってから履歴を引く
            let id = unsafe { fs::FSEventsGetCurrentEventId() };
            std::fs::write(&a[3], b"x").expect("write");
            std::thread::sleep(Duration::from_millis(a[4].parse().expect("ms")));
            let (acc, _) = since(&a[2], id);
            let hit = acc.paths.iter().any(|p| p.ends_with(a[3].rsplit('/').next().unwrap()));
            println!("{}", if hit { "hit" } else { "miss" });
        }
        Some("barrier") => {
            // 別の書き手を模して target を書き、直後に目印つきで履歴を引く
            let (dir, target, sdir) = (&a[2], &a[3], &a[4]);
            let id0 = unsafe { fs::FSEventsGetCurrentEventId() };
            std::fs::write(target, b"x").expect("write");
            let sentinel = format!("{sdir}/sentinel");
            let (acc, el, seen) = since_barrier(&[dir, sdir], &sentinel, id0);
            let hit = acc.paths.iter().any(|p| p == target);
            println!("{} {} {}", if hit { "hit" } else { "miss" }, el.as_micros(), seen);
        }
        _ => {
            eprintln!("usage: fsevents-probe now | since <dir> <id>");
            std::process::exit(2);
        }
    }
}
