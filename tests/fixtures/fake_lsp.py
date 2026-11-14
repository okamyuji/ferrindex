# 試験用の偽の言語サーバー。FAKE_MODE で振る舞いを変える。
#   ok            通常の応答
#   needs_answers references の前にサーバー側の要求を2つ送り、正しい返事が無ければ答えない
#   garbage       ヘッダの無い行を出して黙る
#   hang          何も答えない
#   loading       initialized で読み込み中（quiescent: false）を知らせ、最初の references に空で答えてから読み込み終わりを知らせる
# FAKE_PID_FILE があれば自分の pid を書く。FAKE_EXIT_FILE があれば shutdown の後の exit で書く。
# FAKE_FAIL に method 名があれば、その要求にエラーで答える。
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
    out.write(b"An error has occurred. See the log file\n")
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
        if mode == "needs_answers":
            wait_for_answers()
        if not loaded:
            reply(m, [])
            loaded = True
            status(True)
            continue
        here = {"uri": params["textDocument"]["uri"],
                "range": rng(params["position"]["line"], params["position"]["character"])}
        reply(m, [here, {"uri": root + "/src/a%20b.rs", "range": rng(9, 0)}])
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
