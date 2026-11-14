"""v0.4 の印の手順を全割り込み順で確かめる模型。

共有状態 dirty（全ルート共通の印の数）、synced（取り込んだ印の位置）、
cur（作業ツリー 0=変更前 1=変更後）、idx（索引の状態）、lock（ロックの持ち主）。
プロセス T 編集系ツール（pre印、書き込み、post印。pre/post は失敗しうる）、
B post 成功で起動する背景 sync（試行ロック）、S0 並行する別の検索、
P T の後の次の発話の印、S1 判定対象の検索。
判定 S1 の比較が T の post 印か P の印より後なら、答えは変更後でなければならない。
"""
import copy
import itertools
import sys
from dataclasses import dataclass, field


@dataclass
class St:
    dirty: int = 1
    synced: int = 1
    cur: int = 0
    idx: int = 0
    lock: str | None = None
    loc: dict = field(default_factory=dict)
    ev: list = field(default_factory=list)
    pc: dict = field(default_factory=dict)
    gone: set = field(default_factory=set)
    result: int | None = None


SYNC = ['lock_try', 'readL', 'walk', 'commit', 'unlock']
SEARCH = ['check', 'lock', 'recheck', 'readL', 'walk', 'commit', 'unlock', 'answer']


def need_sync(st, cmp):
    return st.dirty != st.synced if cmp == '!=' else st.dirty > st.synced


def jump_to_answer(st, p, procs):
    st.pc[p] = len(procs[p]) - 1


def make_actions(pre_ok, post_ok, cmp, procs):
    def t_pre(st, p):
        st.dirty += pre_ok

    def t_write(st, p):
        st.cur = 1

    def t_post(st, p):
        st.dirty += post_ok
        st.ev.append('T.post' if post_ok else 'T.lost')
        if not post_ok:
            st.gone.add('B')

    def p_mark(st, p):
        st.dirty += 1
        st.ev.append('P.mark')

    def check(st, p):
        st.ev.append(p + '.check')
        if not need_sync(st, cmp):
            jump_to_answer(st, p, procs)

    def lock_try(st, p):
        if st.lock is not None:
            st.gone.add(p)
        else:
            st.lock = p

    def lock(st, p):
        st.lock = p

    def recheck(st, p):
        if not need_sync(st, cmp):
            st.lock = None
            jump_to_answer(st, p, procs)

    def read_l(st, p):
        st.loc[p + 'L'] = st.dirty

    def walk(st, p):
        st.loc[p + 'W'] = st.cur

    def commit(st, p):
        st.idx, st.synced = st.loc[p + 'W'], st.loc[p + 'L']

    def unlock(st, p):
        st.lock = None

    def answer(st, p):
        if p == 'S1':
            st.result = st.idx

    return {'pre': t_pre, 'write': t_write, 'post': t_post, 'mark': p_mark,
            'check': check, 'lock_try': lock_try, 'lock': lock, 'recheck': recheck,
            'readL': read_l, 'walk': walk, 'commit': commit, 'unlock': unlock,
            'answer': answer}


def enabled(st, p, procs):
    i = st.pc.get(p, 0)
    if i >= len(procs[p]) or p in st.gone:
        return None
    blocked = {
        'B': 'T.post' not in st.ev,
        'P': 'T.post' not in st.ev and 'T.lost' not in st.ev,
    }.get(p, False)
    if blocked or (procs[p][i] == 'lock' and st.lock is not None):
        return None
    return procs[p][i]


def violates(st):
    if st.result is None:
        return False
    ci = st.ev.index('S1.check')
    must = any(e in st.ev[:ci] for e in ('T.post', 'P.mark'))
    return must and st.result != 1


def run(pre_ok, post_ok, with_s0, with_p, cmp):
    procs = {'T': ['pre', 'write', 'post'], 'B': SYNC, 'S1': SEARCH}
    if with_s0:
        procs['S0'] = SEARCH
    if with_p:
        procs['P'] = ['mark']
    acts = make_actions(pre_ok, post_ok, cmp, procs)
    stack, ends, bad = [St()], 0, []
    while stack:
        st = stack.pop()
        moves = [(p, a) for p in procs if (a := enabled(st, p, procs))]
        if not moves:
            ends += 1
            if violates(st):
                bad.append(st.ev)
            continue
        for p, a in moves:
            nxt = copy.deepcopy(st)
            nxt.pc[p] = nxt.pc.get(p, 0) + 1
            acts[a](nxt, p)
            stack.append(nxt)
    return ends, bad


if __name__ == '__main__':
    cmp = sys.argv[1] if len(sys.argv) > 1 else '!='
    for v in itertools.product([True, False], repeat=4):
        ends, bad = run(*v, cmp)
        print(f"pre={v[0]!s:5} post={v[1]!s:5} 並行検索={v[2]!s:5} 次の発話={v[3]!s:5} "
              f"終端={ends:6d} 違反={len(bad)}" + (f" 例={bad[0]}" if bad else ''))
