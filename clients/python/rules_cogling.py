#!/usr/bin/env python3
"""Rule-based cogling for Tic-Tac-Toe and Connect Four, working from observations only.

Tic-Tac-Toe: win, block, centre, corners, edges.
Connect Four: win, block, avoid handing the opponent a win, prefer the centre.

    python3 rules_cogling.py --game connect4 --games 5
"""

from __future__ import annotations

import random
from typing import Optional

from cogling_client import Cogling, Decision, GameInfo, main_args, run

TTT_LINES = [(0, 1, 2), (3, 4, 5), (6, 7, 8), (0, 3, 6), (1, 4, 7), (2, 5, 8), (0, 4, 8), (2, 4, 6)]


def ttt_wins(board: list, cell: int, mark: str) -> bool:
    if board[cell] is not None:
        return False
    trial = list(board)
    trial[cell] = mark
    return any(all(trial[i] == mark for i in line) for line in TTT_LINES)


def ttt_move(board: list, mark: str, rng: random.Random) -> Optional[int]:
    other = "O" if mark == "X" else "X"
    empty = [i for i, c in enumerate(board) if c is None]
    for target in (mark, other):
        for cell in empty:
            if ttt_wins(board, cell, target):
                return cell
    for group in ([4], [0, 2, 6, 8], [1, 3, 5, 7]):
        options = [c for c in group if c in empty]
        if options:
            return rng.choice(options)
    return None


class C4Board:
    """Connect Four board from an observation: ``cells[row * columns + column]``, row 0 at the bottom."""

    def __init__(self, board: dict):
        self.columns, self.rows = board["columns"], board["rows"]
        self.cells = list(board["cells"])

    def get(self, c: int, r: int):
        if 0 <= c < self.columns and 0 <= r < self.rows:
            return self.cells[r * self.columns + c]
        return None

    def landing_row(self, c: int) -> Optional[int]:
        return next((r for r in range(self.rows) if self.get(c, r) is None), None)

    def open_columns(self) -> list:
        return [c for c in range(self.columns) if self.landing_row(c) is not None]

    def drop(self, c: int, seat: int) -> Optional["C4Board"]:
        r = self.landing_row(c)
        if r is None:
            return None
        nxt = C4Board({"columns": self.columns, "rows": self.rows, "cells": self.cells})
        nxt.cells[r * self.columns + c] = seat
        nxt.last = (c, r)
        return nxt

    def wins(self, c: int, seat: int) -> bool:
        nxt = self.drop(c, seat)
        if nxt is None:
            return False
        col, row = nxt.last
        for dc, dr in ((1, 0), (0, 1), (1, 1), (1, -1)):
            run = 1
            for sign in (1, -1):
                k = 1
                while nxt.get(col + sign * k * dc, row + sign * k * dr) == seat:
                    run += 1
                    k += 1
            if run >= 4:
                return True
        return False


def c4_move(board: C4Board, me: int, rng: random.Random) -> Optional[int]:
    them = 1 - me
    open_cols = board.open_columns()
    for target in (me, them):
        for c in open_cols:
            if board.wins(c, target):
                return c

    def gives_away(c: int) -> bool:
        nxt = board.drop(c, me)
        return nxt is not None and nxt.wins(c, them)

    pool = [c for c in open_cols if not gives_away(c)] or open_cols
    if not pool:
        return None
    centre = board.columns - 1
    best = min(abs(2 * c - centre) for c in pool)
    return rng.choice([c for c in pool if abs(2 * c - centre) == best])


class RulesCogling(Cogling):
    def __init__(self, seed: int):
        self.rng = random.Random(seed)
        self.game = None

    def on_game_start(self, info: GameInfo) -> None:
        if info.game not in ("tictactoe", "connect4"):
            raise ValueError(f"rules cogling does not know {info.game}")
        self.game = info.game

    def decide(self, decision: Decision):
        obs = decision.observation
        if self.game == "tictactoe":
            return {"cell": ttt_move(obs["board"], obs["mark"], self.rng)}
        return {"column": c4_move(C4Board(obs["board"]), obs["seat"], self.rng)}


def main() -> int:
    args = main_args(__doc__)
    results = run(
        RulesCogling(args.seed),
        args.url,
        name=args.name or "python-rules",
        version=args.version,
        game=args.game,
        join=args.join,
        games=args.games,
    )
    for r in results:
        print(f"{r['game_id']} as seat {r['seat']}: {r['result']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
