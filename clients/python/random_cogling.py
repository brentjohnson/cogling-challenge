#!/usr/bin/env python3
"""Plays uniformly random legal actions in any game (needs state_and_legal mode).

    python3 random_cogling.py --game tictactoe --games 3
"""

import random

from cogling_client import Cogling, Decision, ProtocolError, main_args, run


class RandomCogling(Cogling):
    def __init__(self, seed: int):
        self.rng = random.Random(seed)

    def decide(self, decision: Decision):
        if not decision.legal_actions:
            raise ProtocolError("random cogling needs legal actions (observation mode state_and_legal)")
        return self.rng.choice(decision.legal_actions)


def main() -> int:
    args = main_args(__doc__)
    results = run(
        RandomCogling(args.seed),
        args.url,
        name=args.name or "python-random",
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
