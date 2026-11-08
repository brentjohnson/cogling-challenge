"""Unit tests for the Python client and sample coglings: python3 -m unittest -v"""

import io
import random
import unittest

from cogling_client import _OP_TEXT, decode_frame, encode_frame
from rules_cogling import C4Board, c4_move, ttt_move


class FrameTests(unittest.TestCase):
    def round_trip(self, payload: bytes):
        frame = encode_frame(_OP_TEXT, payload, mask_key=b"\x01\x02\x03\x04")
        fin, opcode, decoded = decode_frame(io.BytesIO(frame).read)
        self.assertTrue(fin)
        self.assertEqual(opcode, _OP_TEXT)
        self.assertEqual(decoded, payload)

    def test_lengths(self):
        for n in (0, 5, 125, 126, 65535, 65536):
            self.round_trip(b"x" * n)

    def test_masking_is_applied(self):
        frame = encode_frame(_OP_TEXT, b"hello", mask_key=b"\xff\xff\xff\xff")
        self.assertNotIn(b"hello", frame)

    def test_truncated_frame(self):
        frame = encode_frame(_OP_TEXT, b"hello")[:-1]
        with self.assertRaises(ConnectionError):
            decode_frame(io.BytesIO(frame).read)


class TicTacToeRulesTests(unittest.TestCase):
    def test_win_then_block_then_centre(self):
        rng = random.Random(0)
        board = ["X", "X", None, "O", "O", None, None, None, None]
        self.assertEqual(ttt_move(board, "X", rng), 2)
        self.assertEqual(ttt_move(board, "O", rng), 5)
        self.assertEqual(ttt_move([None] * 9, "X", rng), 4)
        self.assertIsNone(ttt_move(["X", "O", "X", "X", "O", "O", "O", "X", "X"], "X", rng))


def c4(moves, columns=7, rows=6):
    board = C4Board({"columns": columns, "rows": rows, "cells": [None] * (columns * rows)})
    for column, seat in moves:
        board = board.drop(column, seat)
    return board


class ConnectFourRulesTests(unittest.TestCase):
    def test_win_and_block(self):
        rng = random.Random(0)
        board = c4([(0, 0), (6, 1), (1, 0), (6, 1), (2, 0)])
        self.assertEqual(c4_move(board, 0, rng), 3)
        self.assertEqual(c4_move(board, 1, rng), 3)

    def test_avoids_giving_away_a_win(self):
        rng = random.Random(0)
        board = c4([(0, 0), (1, 1), (2, 0), (0, 1), (1, 1), (2, 1), (6, 0), (6, 0)])
        for _ in range(20):
            self.assertNotEqual(c4_move(board, 0, rng), 3)

    def test_prefers_centre(self):
        self.assertEqual(c4_move(c4([]), 0, random.Random(0)), 3)

    def test_vertical_and_diagonal_detection(self):
        board = c4([(0, 0), (0, 0), (0, 0)])
        self.assertTrue(board.wins(0, 0))
        diag = c4([(0, 0), (1, 1), (1, 0), (2, 1), (2, 1), (2, 0), (3, 1), (3, 1), (3, 1)])
        self.assertTrue(diag.wins(3, 0))


if __name__ == "__main__":
    unittest.main()
