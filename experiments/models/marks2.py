"""v0.5 の Edit/Write の速い経路（保留の印）を全割り込み順で確かめる模型。

共有状態 dirty/synced（全体の印）、pend（保留の印の集合）、cur/idx、lock。
E  Edit: pre で保留の印 -> 書き込み -> post（ロックを試行し、取れたら1ファイルを取り込んで保留を消す）
   pre/post は失敗しうる。post がロックを取れなければ保留を残して終わる
S0 並行する検索、S1 判定する検索、P 次の印（Stop か次の発話。全体の印）
検索 dirty!=synced か保留があればロックを待ち、もう一度比べ、必要なら全体の sync
sync ロック後、走査の前に dirty と保留の一覧を読み、取り込み後に synced と読んだ保留だけを消す
判定 S1 の比較が E の post 完了か P より後なら、答えは変更後でなければならない
使い方 python3 -I marks2.py [破壊する規則 none|clear_all|post_bg]
  clear_all  sync が走査後の保留まで消す（誤り）
  no_pend    検索が保留を見ない（誤り）
  busy_nomark  post がロックを取れないとき全体の印を書かない（v0.5 の初案）
  第2引数 strict で、post が失われたかロックが取れなかった後の検索も判定する
"""
import copy
import itertools
import sys
from dataclasses import dataclass, field

MUT = sys.argv[1] if len(sys.argv) > 1 else 'none'
STRICT = len(sys.argv) > 2 and sys.argv[2] == 'strict'


@dataclass
class St:
    dirty: int = 1
    synced: int = 1
    pend: frozenset = frozenset()
    cur: int = 0
    idx: int = 0
    lock: str | None = None
    loc: dict = field(default_factory=dict)
    ev: list = field(default_factory=list)
    pc: dict = field(default_factory=dict)
    gone: set = field(default_factory=set)
    result: int | None = None


SEARCH = ['check', 'lock', 'recheck', 'readL', 'walk', 'commit', 'unlock', 'answer']


def needs(st):
    return st.dirty != st.synced or (bool(st.pend) and MUT != 'no_pend')


def to_answer(st, p, procs):
    st.pc[p] = len(procs[p]) - 1


def edit_actions(pre_ok, post_ok):
    def pre(st, p):
        if pre_ok:
            st.pend = st.pend | {'e'}

    def write(st, p):
        st.cur = 1

    def post_lock(st, p):
        if not post_ok:
            st.ev.append('E.lost')
            st.gone.add(p)
        elif st.lock is not None:
            st.ev.append('E.busy')  # ロックが取れないので全体の印を書く
            if MUT != 'busy_nomark':
                st.dirty += 1
            st.gone.add(p)
        else:
            st.lock = p

    def post_read(st, p):
        st.loc['Ecur'] = st.cur

    def post_commit(st, p):
        st.idx = st.loc['Ecur']
        st.pend = st.pend - {'e'}
        st.lock = None
        st.ev.append('E.done')

    return {'pre': pre, 'write': write, 'post_lock': post_lock,
            'post_read': post_read, 'post_commit': post_commit}


def search_actions(procs):
    def check(st, p):
        st.ev.append(p + '.check')
        if not needs(st):
            to_answer(st, p, procs)

    def lock(st, p):
        st.lock = p

    def recheck(st, p):
        if not needs(st):
            st.lock = None
            to_answer(st, p, procs)

    def read_l(st, p):
        st.loc[p + 'L'] = st.dirty
        st.loc[p + 'P'] = st.pend

    def walk(st, p):
        st.loc[p + 'W'] = st.cur

    def commit(st, p):
        st.idx, st.synced = st.loc[p + 'W'], st.loc[p + 'L']
        st.pend = frozenset() if MUT == 'clear_all' else st.pend - st.loc[p + 'P']

    def unlock(st, p):
        st.lock = None

    def answer(st, p):
        if p == 'S1':
            st.result = st.idx

    def mark(st, p):
        st.dirty += 1
        st.ev.append('P.mark')

    return {'check': check, 'lock': lock, 'recheck': recheck, 'readL': read_l,
            'walk': walk, 'commit': commit, 'unlock': unlock, 'answer': answer,
            'mark': mark}


def enabled(st, p, procs):
    i = st.pc.get(p, 0)
    if i >= len(procs[p]) or p in st.gone:
        return None
    if p == 'P' and not any(e in st.ev for e in ('E.done', 'E.lost', 'E.busy')):
        return None
    if procs[p][i] == 'lock' and st.lock is not None:
        return None
    return procs[p][i]


def violates(st):
    if st.result is None:
        return False
    ci = st.ev.index('S1.check')
    must = any(e in st.ev[:ci] for e in ('E.done', 'P.mark') + (('E.lost', 'E.busy') if STRICT else ()))
    return must and st.result != 1


def run(pre_ok, post_ok, with_s0, with_p):
    procs = {'E': ['pre', 'write', 'post_lock', 'post_read', 'post_commit'], 'S1': SEARCH}
    if with_s0:
        procs['S0'] = SEARCH
    if with_p:
        procs['P'] = ['mark']
    acts = {**edit_actions(pre_ok, post_ok), **search_actions(procs)}
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
    total = 0
    for v in itertools.product([True, False], repeat=4):
        ends, bad = run(*v)
        total += len(bad)
        print(f"pre={v[0]!s:5} post={v[1]!s:5} 並行検索={v[2]!s:5} 次の印={v[3]!s:5} "
              f"終端={ends:6d} 違反={len(bad)}" + (f" 例={bad[0]}" if bad else ''))
    print(f"規則={MUT} 違反の合計={total}")
