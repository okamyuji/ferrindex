# 試験用の偽の言語サーバー。FAKE_MODE で振る舞いを変える。
#   ok            通常の応答
#   needs_answers references の前にサーバー側の要求を2つ送り、正しい返事が無ければ答えない
#   garbage       ヘッダの無い行（FAKE_GARBAGE があればその文字列）を出して黙る
#   hang          何も答えない
#   unsorted      references に重複を含む順不同の結果を返す
#   growing       references の答えが問うたびに1件ずつ増え、2件で止まる
#   changing      references の答えが問うたびに1件ずつ増え続ける
#   loading       initialized で読み込み中（quiescent: false）を知らせ、最初の references に空で答えてから読み込み終わりを知らせる
# FAKE_PID_FILE があれば自分の pid を書く。FAKE_EXIT_FILE があれば shutdown の後の exit で書く。
# FAKE_FAIL に method 名があれば、その要求にエラーで答える。
# FAKE_REFS は references、implementation、supertypes の答え（[[パス, 0始まりの行, 0始まりの列], ...] の JSON）。相対パスは根からの相対。
# references では {"問うた行 列": [...]} の形で、問うた位置ごとに答えを変えられる。
# FAKE_REFS_RUN は references の答えを「パス 0始まりの行 最初の列 列の間隔 件数」で並べる。
# 1つの環境変数に入りきらない数の答えを返すときに使う。
# FAKE_LOG があれば、位置を問う要求ごとに「method 行 列」を1行足す。
import json
import os
import sys
import time

mode = os.environ.get("FAKE_MODE", "ok")
if os.environ.get("FAKE_PID_FILE"):
    with open(os.environ["FAKE_PID_FILE"], "w") as f:
        f.write(str(os.getpid()))

out = sys.stdout.buffer
inp = sys.stdin.buffer

if mode == "garbage":
    out.write(os.environ.get("FAKE_GARBAGE", "An error has occurred. See the log file").encode() + b"\n")
    out.flush()
    time.sleep(60)
    sys.exit(1)
if mode == "hang":
    time.sleep(60)
    sys.exit(1)


def send(msg):
    body = json.dumps(msg).encode()
    out.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    out.flush()


def recv():
    length = None
    while True:
        line = inp.readline()
        if not line:
            sys.exit(0)
        line = line.strip()
        if not line:
            break
        key, value = line.split(b":", 1)
        if key.lower() == b"content-length":
            length = int(value)
    return json.loads(inp.read(length))


def reply(msg, result):
    send({"jsonrpc": "2.0", "id": msg["id"], "result": result})


def rng(line, character):
    pos = {"line": line, "character": character}
    return {"start": pos, "end": pos}


answers = {}


def wait_for_answers():
    # 2 は references の要求の id と重なる。クライアントは method の有無で区別する必要がある。
    send({"jsonrpc": "2.0", "id": "p1", "method": "window/workDoneProgress/create", "params": {"token": "t"}})
    send({"jsonrpc": "2.0", "id": 2, "method": "workspace/configuration",
          "params": {"items": [{"section": "a"}, {"section": "b"}]}})
    while "p1" not in answers or 2 not in answers:
        m = recv()
        if "method" not in m:
            answers[m["id"]] = m
    progress, config = answers["p1"], answers[2]
    if not ("result" in progress and progress["result"] is None and config.get("result") == [None, None]):
        time.sleep(60)
        sys.exit(1)


def status(quiescent):
    send({"jsonrpc": "2.0", "method": "experimental/serverStatus", "params": {"health": "ok", "quiescent": quiescent}})


root = None
seen_ids = set()
opened = set()
shut = False
loaded = mode != "loading"
asked = 0
while True:
    m = recv()
    method = m.get("method")
    params = m.get("params") or {}
    if method is None:
        continue
    if "id" in m and m["id"] in seen_ids:
        # JSON-RPC の要求の id は重ならない。重なれば shutdown も受け付けない。
        send({"jsonrpc": "2.0", "id": m["id"], "error": {"code": -32600, "message": "duplicate id"}})
        continue
    if "id" in m:
        seen_ids.add(m["id"])
    if "id" in m and method == os.environ.get("FAKE_FAIL"):
        send({"jsonrpc": "2.0", "id": m["id"], "error": {"code": -32603, "message": "failed " + method}})
    elif method == "initialize":
        root = params["rootUri"]
        reply(m, {"capabilities": {}})
    elif method == "initialized":
        if not loaded:
            status(False)
    elif method == "textDocument/didOpen":
        opened.add(params["textDocument"]["uri"])
    elif method == "shutdown":
        shut = True
        reply(m, None)
    elif method == "exit":
        if shut and os.environ.get("FAKE_EXIT_FILE"):
            # 猶予なしで止められたら書けないように、書く前に待つ。
            time.sleep(0.3)
            with open(os.environ["FAKE_EXIT_FILE"], "w") as f:
                f.write("exit")
        sys.exit(0)
    elif "id" not in m:
        continue
    elif "textDocument" in params and params["textDocument"]["uri"] not in opened:
        send({"jsonrpc": "2.0", "id": m["id"], "error": {"code": -32602, "message": "not opened"}})
    elif method == "textDocument/references":
        if os.environ.get("FAKE_LOG"):
            with open(os.environ["FAKE_LOG"], "a") as f:
                pos = params["position"]
                f.write("%s %d %d\n" % (method, pos["line"], pos["character"]))
        if os.environ.get("FAKE_REFS_RUN"):
            p, line, ch, step, n = os.environ["FAKE_REFS_RUN"].split()
            reply(m, [{"uri": root + "/" + p, "range": rng(int(line), int(ch) + i * int(step))}
                      for i in range(int(n))])
            continue
        if os.environ.get("FAKE_REFS"):
            refs = json.loads(os.environ["FAKE_REFS"])
            if isinstance(refs, dict):
                pos = params["position"]
                refs = refs.get("%d %d" % (pos["line"], pos["character"]), [])
            reply(m, [{"uri": p if p.startswith("file:") else root + "/" + p, "range": rng(line, ch)}
                      for p, line, ch in refs])
            continue
        asked += 1
        if mode in ("growing", "changing"):
            n = asked - 1 if mode == "changing" else min(asked - 1, 2)
            reply(m, [{"uri": root + "/x.rs", "range": rng(i, 0)} for i in range(n)])
            continue
        if mode == "needs_answers":
            wait_for_answers()
        if not loaded:
            reply(m, [])
            loaded = True
            status(True)
            continue
        if mode == "unsorted":
            b, a = root + "/b.rs", root + "/a.rs"
            reply(m, [{"uri": u, "range": rng(line, ch)} for u, line, ch in
                      [(b, 1, 0), (a, 4, 2), (b, 1, 0), (a, 4, 1), (a, 0, 9)]])
            continue
        here = {"uri": params["textDocument"]["uri"],
                "range": rng(params["position"]["line"], params["position"]["character"])}
        reply(m, [here, {"uri": root + "/src/a%20b.rs", "range": rng(9, 0)}])
    elif method == "textDocument/implementation" and os.environ.get("FAKE_REFS"):
        reply(m, [{"uri": p if p.startswith("file:") else root + "/" + p, "range": rng(line, ch)}
                  for p, line, ch in json.loads(os.environ["FAKE_REFS"])])
    elif method == "typeHierarchy/supertypes" and os.environ.get("FAKE_REFS"):
        reply(m, [{"name": "S", "uri": root + "/" + p, "selectionRange": rng(line, ch)}
                  for p, line, ch in json.loads(os.environ["FAKE_REFS"])])
    elif method == "textDocument/implementation":
        reply(m, [{"targetUri": root + "/impl.rs", "targetRange": rng(0, 0), "targetSelectionRange": rng(3, 5)}])
    elif method == "textDocument/prepareCallHierarchy":
        reply(m, [{"name": "f", "uri": params["textDocument"]["uri"], "selectionRange": rng(0, 0)}])
    elif method == "callHierarchy/incomingCalls":
        assert params["item"]["name"] == "f"
        reply(m, [{"from": {"name": "g", "uri": root + "/caller.rs", "selectionRange": rng(0, 0)},
                   "fromRanges": [rng(4, 1), rng(6, 2)]}])
    elif method == "textDocument/prepareTypeHierarchy":
        reply(m, [{"name": "T", "uri": params["textDocument"]["uri"], "selectionRange": rng(0, 0)}])
    elif method == "typeHierarchy/supertypes":
        assert params["item"]["name"] == "T"
        reply(m, [{"name": "S", "uri": root + "/base.rs", "selectionRange": rng(1, 7)}])
    else:
        send({"jsonrpc": "2.0", "id": m["id"], "error": {"code": -32601, "message": "unknown " + method}})
