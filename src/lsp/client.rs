use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::slice;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::servers::language_id;
use super::{Location, LspError, Method, ServerSpec};
use crate::lang::Lang;

const POLL: Duration = Duration::from_millis(50);
/// `exit` の後に自分で終わるのを待つ回数。POLL と掛けて2秒。
const GRACE_POLLS: u32 = 40;

/// `query` と同じだが、起動するサーバーを指定する。
#[allow(clippy::too_many_arguments)]
pub fn query_with(
    spec: &ServerSpec,
    root: &Path,
    lang: Lang,
    file: &Path,
    line: u32,
    col: u32,
    method: Method,
    deadline: Instant,
) -> Result<Vec<Location>, LspError> {
    // サーバーは正規化したパスで答えるので、送るパスも正規化する（/var と /private/var など）。
    let root = fs::canonicalize(root).map_err(|e| start_failed(root, e))?;
    let file = root.join(file);
    let file = fs::canonicalize(&file).map_err(|e| start_failed(&file, e))?;
    let text = fs::read_to_string(&file).map_err(|e| start_failed(&file, e))?;
    let mut server = Server::spawn(spec, &root)?;
    let result = server.initialize(&root, deadline).and_then(|()| {
        let uri = to_uri(&file);
        let doc = json!({"uri": uri, "languageId": language_id(lang), "version": 1, "text": text});
        server.notify("textDocument/didOpen", json!({ "textDocument": doc }))?;
        let at = json!({
            "textDocument": {"uri": uri},
            "position": {"line": line.saturating_sub(1), "character": col.saturating_sub(1)},
        });
        server.settled(method, &at, deadline)
    });
    server.stop(result == Err(LspError::Timeout));
    // csharp-ls などは同じ結果を呼ぶたびに違う順で返し、重複も含みうる。
    result.map(|mut found| {
        found.sort();
        found.dedup();
        found
    })
}

fn start_failed(path: &Path, e: io::Error) -> LspError {
    LspError::StartFailed(format!("{}: {e}", path.display()))
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    inbox: Receiver<Result<Value, LspError>>,
    next_id: u64,
    /// rust-analyzer は読み込みが終わる前の問い合わせに空で答える。読み込み中かを
    /// `experimental/serverStatus` の `quiescent` で知る（送らないサーバーでは常に false）。
    loading: bool,
}

impl Server {
    fn spawn(spec: &ServerSpec, root: &Path) -> Result<Server, LspError> {
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // 孫プロセス（jdtls の java、tsserver など）もまとめて止めるため、グループを分ける。
            .process_group(0)
            .spawn()
            .map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => LspError::NotInstalled(spec.name.into()),
                _ => LspError::StartFailed(e.to_string()),
            })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            unreachable!("stdin と stdout は piped で開いている");
        };
        let (tx, inbox) = mpsc::channel();
        // 読み手を分けるのは、主スレッドが recv_timeout で期限を守るため。
        thread::spawn(move || read_loop(BufReader::new(stdout), &tx));
        Ok(Server {
            child,
            stdin,
            inbox,
            next_id: 0,
            loading: false,
        })
    }

    fn send(&mut self, msg: &Value) -> Result<(), LspError> {
        // shortcut: 書き込みは主スレッドで行う。stdin を読まなくなったサーバーへ大きな didOpen を
        // 送ると期限を越えて止まりうる。実際に起きたら書き込み専用のスレッドに替える。
        let body = msg.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len())
            .and_then(|()| self.stdin.flush())
            .map_err(|e| LspError::Protocol(e.to_string()))
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), LspError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, LspError> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let Some(msg) = self.pump(deadline)? else {
                continue;
            };
            if msg["id"] != id {
                continue;
            }
            return match msg.get("error") {
                Some(e) => Err(LspError::Protocol(
                    e["message"]
                        .as_str()
                        .map_or_else(|| e.to_string(), str::to_string),
                )),
                None => Ok(msg["result"].clone()),
            };
        }
    }

    /// 1通読む。応答なら返し、サーバーからの要求には答え、通知は状態だけ覚えて None を返す。
    fn pump(&mut self, deadline: Instant) -> Result<Option<Value>, LspError> {
        let left = deadline.saturating_duration_since(Instant::now());
        // 読み手は終わる前に必ず理由を送るので、切断（Err）がここに来るのは期限切れだけ。
        let msg = self
            .inbox
            .recv_timeout(left)
            .map_err(|_| LspError::Timeout)??;
        // サーバーからの要求の id は自分の id と重なりうるので、先に method で見分ける。
        let Some(method) = msg["method"].as_str() else {
            return Ok(Some(msg));
        };
        if method == "experimental/serverStatus" {
            self.loading = msg["params"]["quiescent"] == false;
        }
        if msg.get("id").is_some() {
            self.send(&reply_to(&msg))?;
        }
        Ok(None)
    }

    fn initialize(&mut self, root: &Path, deadline: Instant) -> Result<(), LspError> {
        let uri = to_uri(root);
        let params = json!({
            "processId": std::process::id(),
            "rootUri": uri,
            "workspaceFolders": [{"uri": uri, "name": "root"}],
            "capabilities": {
                "textDocument": {
                    "synchronization": {},
                    "references": {},
                    "implementation": {"linkSupport": true},
                    "callHierarchy": {},
                    "typeHierarchy": {},
                },
                "window": {"workDoneProgress": true},
                "experimental": {"serverStatusNotification": true},
                "workspace": {"configuration": true, "workspaceFolders": true},
            },
        });
        match self.request("initialize", params, deadline) {
            Err(LspError::Protocol(why)) => return Err(LspError::StartFailed(why)),
            other => other?,
        };
        self.notify("initialized", json!({}))
    }

    /// 読み込み中に得た答えは捨て、読み込みが終わってから問い直す。
    fn settled(
        &mut self,
        method: Method,
        at: &Value,
        deadline: Instant,
    ) -> Result<Vec<Location>, LspError> {
        loop {
            while self.loading {
                self.pump(deadline)?;
            }
            let got = self.locations(method, at.clone(), deadline)?;
            if !self.loading {
                return Ok(got);
            }
        }
    }

    fn locations(
        &mut self,
        method: Method,
        at: Value,
        deadline: Instant,
    ) -> Result<Vec<Location>, LspError> {
        match method {
            Method::References => {
                let mut params = at;
                params["context"] = json!({"includeDeclaration": false});
                Ok(locations(&self.request(
                    "textDocument/references",
                    params,
                    deadline,
                )?))
            }
            Method::Implementation => Ok(locations(&self.request(
                "textDocument/implementation",
                at,
                deadline,
            )?)),
            Method::IncomingCalls => self.hierarchy(
                [
                    "textDocument/prepareCallHierarchy",
                    "callHierarchy/incomingCalls",
                ],
                at,
                deadline,
                call_sites,
            ),
            Method::Supertypes => self.hierarchy(
                [
                    "textDocument/prepareTypeHierarchy",
                    "typeHierarchy/supertypes",
                ],
                at,
                deadline,
                items,
            ),
        }
    }

    /// 位置から項目を求め（prepare）、項目ごとに関係を問い合わせる。
    fn hierarchy(
        &mut self,
        [prepare, then]: [&str; 2],
        at: Value,
        deadline: Instant,
        read: fn(&Value) -> Vec<Location>,
    ) -> Result<Vec<Location>, LspError> {
        let prepared = self.request(prepare, at, deadline)?;
        let mut out = Vec::new();
        for item in prepared.as_array().into_iter().flatten() {
            out.extend(read(&self.request(
                then,
                json!({ "item": item }),
                deadline,
            )?));
        }
        Ok(out)
    }

    /// 期限切れなら猶予を置かずに止める。答えないサーバーを待つと期限を越えるため。
    fn stop(mut self, timed_out: bool) {
        self.next_id += 1;
        let _ = self.send(&json!({"jsonrpc": "2.0", "id": self.next_id, "method": "shutdown"}));
        let _ = self.notify("exit", Value::Null);
        drop(self.stdin);
        if !timed_out {
            for _ in 0..GRACE_POLLS {
                if !matches!(self.child.try_wait(), Ok(None)) {
                    break;
                }
                thread::sleep(POLL);
            }
        }
        // shortcut: kill コマンドを使う、libc を足す判断がされたら killpg に替える。
        // 先頭のプロセスが終わっていても、グループに残った孫を止めるために毎回送る。
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{}", self.child.id())])
            .stderr(Stdio::null())
            .status();
        let _ = self.child.wait();
    }
}

/// 終わる前に必ず理由を1つ送る。最初の応答より前の失敗は起動の失敗とみなす。
fn read_loop(mut r: impl BufRead, tx: &Sender<Result<Value, LspError>>) {
    let mut started = false;
    let why = loop {
        match read_message(&mut r) {
            Ok(Some(msg)) => {
                started = true;
                if tx.send(Ok(msg)).is_err() {
                    return;
                }
            }
            Ok(None) => break "サーバーが終了しました".to_string(),
            Err(why) => break why,
        }
    };
    let _ = tx.send(Err(if started {
        LspError::Protocol(why)
    } else {
        LspError::StartFailed(why)
    }));
}

/// ヘッダでない行は、その行を理由にして失敗にする（jdtls は起動に失敗すると素の文を出す）。
fn read_message(r: &mut impl BufRead) -> Result<Option<Value>, String> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        match line.split_once(':') {
            Some((k, v)) if k.eq_ignore_ascii_case("content-length") => {
                len = Some(v.trim().parse::<usize>().map_err(|e| e.to_string())?);
            }
            Some((k, _)) if k.eq_ignore_ascii_case("content-type") => {}
            _ => return Err(line.to_string()),
        }
    }
    let mut body = vec![0; len.ok_or("Content-Length がありません")?];
    r.read_exact(&mut body).map_err(|e| e.to_string())?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// サーバーからの要求には null で答える。workspace/configuration だけは項目ごとの配列（null は既定のまま）。
fn reply_to(req: &Value) -> Value {
    let result = match req["method"].as_str() {
        Some("workspace/configuration") => {
            let n = req["params"]["items"].as_array().map_or(0, Vec::len);
            Value::Array(vec![Value::Null; n])
        }
        _ => Value::Null,
    };
    json!({"jsonrpc": "2.0", "id": req["id"].clone(), "result": result})
}

fn to_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(char::from(b));
        } else {
            let _ = write!(uri, "%{b:02X}");
        }
    }
    uri
}

fn from_uri(uri: &str) -> Option<PathBuf> {
    let mut parts = uri.strip_prefix("file://")?.split('%');
    let mut bytes = parts.next()?.as_bytes().to_vec();
    for part in parts {
        bytes.push(u8::from_str_radix(part.get(..2)?, 16).ok()?);
        bytes.extend_from_slice(&part.as_bytes()[2..]);
    }
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

fn location(uri: &Value, range: &Value) -> Option<Location> {
    let one_based = |v: &Value| u32::try_from(v.as_u64()?).ok()?.checked_add(1);
    Some(Location {
        path: from_uri(uri.as_str()?)?,
        line: one_based(&range["start"]["line"])?,
        col: one_based(&range["start"]["character"])?,
    })
}

/// references と implementation の答え。null、Location、Location[]、LocationLink[] のどれも来うる。
fn locations(v: &Value) -> Vec<Location> {
    let all = v.as_array().map_or(slice::from_ref(v), Vec::as_slice);
    all.iter()
        .filter_map(|l| match l.get("targetUri") {
            Some(uri) => location(uri, &l["targetSelectionRange"]),
            None => location(&l["uri"], &l["range"]),
        })
        .collect()
}

/// 型階層の項目。
fn items(v: &Value) -> Vec<Location> {
    let all = v.as_array().map_or(&[][..], Vec::as_slice);
    all.iter()
        .filter_map(|i| location(&i["uri"], &i["selectionRange"]))
        .collect()
}

/// 呼び出し元の答え。呼び出し元の定義ではなく、呼んでいる箇所（fromRanges）を返す。
fn call_sites(v: &Value) -> Vec<Location> {
    let all = v.as_array().map_or(&[][..], Vec::as_slice);
    all.iter()
        .flat_map(|call| {
            let ranges = call["fromRanges"].as_array().map_or(&[][..], Vec::as_slice);
            ranges
                .iter()
                .filter_map(|r| location(&call["from"]["uri"], r))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn framed(body: &str) -> String {
        format!("Content-Length: {}\r\n\r\n{body}", body.len())
    }

    #[test]
    fn read_message_takes_headers_in_any_case_and_skips_content_type() {
        let text = "content-length: 2\r\nContent-Type: x\r\n\r\n{}";
        assert_eq!(read_message(&mut Cursor::new(text)), Ok(Some(json!({}))));
        assert_eq!(read_message(&mut Cursor::new("")), Ok(None));
    }

    #[test]
    fn read_message_rejects_what_is_not_a_header() {
        let err = |t: &str| read_message(&mut Cursor::new(t.to_string())).unwrap_err();
        assert_eq!(err("An error has occurred\n"), "An error has occurred");
        assert_eq!(err("X-Other: 1\r\n\r\n"), "X-Other: 1");
        assert_eq!(err("\r\n"), "Content-Length がありません");
        assert!(err("Content-Length: x\r\n\r\n").contains("invalid digit"));
        assert!(err("Content-Length: 5\r\n\r\n{}").contains("fill whole buffer"));
        assert!(err(&framed("{")).contains("EOF"));
    }

    fn drain(text: String) -> Vec<Result<Value, LspError>> {
        let (tx, rx) = mpsc::channel();
        read_loop(Cursor::new(text), &tx);
        drop(tx);
        rx.into_iter().collect()
    }

    #[test]
    fn read_loop_names_failures_before_and_after_the_first_message() {
        assert_eq!(
            drain(String::new()),
            [Err(LspError::StartFailed("サーバーが終了しました".into()))]
        );
        let after = drain(framed("1") + "oops\n");
        assert_eq!(
            after,
            [Ok(json!(1)), Err(LspError::Protocol("oops".into()))]
        );
    }

    #[test]
    fn read_loop_stops_when_nobody_listens() {
        let (tx, rx) = mpsc::channel();
        drop(rx);
        read_loop(Cursor::new(framed("1")), &tx);
    }

    #[test]
    fn server_requests_get_null_or_one_null_per_item() {
        let config = json!({"id": "c", "method": "workspace/configuration", "params": {"items": [{}, {}, {}]}});
        let want = json!({"jsonrpc": "2.0", "id": "c", "result": [null, null, null]});
        assert_eq!(reply_to(&config), want);
        let other = json!({"id": 4, "method": "client/registerCapability", "params": {}});
        assert_eq!(
            reply_to(&other),
            json!({"jsonrpc": "2.0", "id": 4, "result": null})
        );
    }

    #[test]
    fn uris_escape_and_unescape_bytes() {
        let path = Path::new("/a b/é~_-.rs");
        let uri = to_uri(path);
        assert_eq!(uri, "file:///a%20b/%C3%A9~_-.rs");
        assert_eq!(from_uri(&uri).as_deref(), Some(path));
        assert_eq!(from_uri("file:///x%2"), None);
        assert_eq!(from_uri("file:///x%zz"), None);
        assert_eq!(from_uri("http://x"), None);
    }

    fn at(uri: &str, line: u64, character: u64) -> Value {
        json!({"uri": uri, "range": {"start": {"line": line, "character": character}}})
    }

    #[test]
    fn locations_accept_every_shape_and_skip_broken_entries() {
        let one = Location {
            path: "/a".into(),
            line: 2,
            col: 3,
        };
        assert_eq!(locations(&at("file:///a", 1, 2)), slice::from_ref(&one));
        assert_eq!(locations(&Value::Null), []);
        let link = json!({"targetUri": "file:///a", "targetSelectionRange": {"start": {"line": 1, "character": 2}}});
        let broken = [
            at("file:///a", u64::from(u32::MAX), 0),
            at("x", 0, 0),
            json!({"uri": "file:///a"}),
        ];
        let mut all = vec![link];
        all.extend(broken);
        assert_eq!(locations(&Value::Array(all)), [one]);
    }

    #[test]
    fn hierarchy_answers_ignore_null() {
        assert_eq!(items(&Value::Null), []);
        assert_eq!(call_sites(&Value::Null), []);
        assert_eq!(call_sites(&json!([{"from": {"uri": "file:///a"}}])), []);
    }
}
