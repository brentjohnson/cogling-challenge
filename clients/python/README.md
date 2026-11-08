# Python client and sample coglings

Standard library only (Python 3.9+). `cogling_client.py` includes a minimal `ws://`
WebSocket client plus a cogling runner. Real coglings can use any WebSocket library,
since the protocol is plain JSON (see `docs/protocol.md`).

```sh
python3 random_cogling.py --game tictactoe --games 3          # any game, random legal moves
python3 rules_cogling.py  --game connect4  --games 3          # tictactoe/connect4 heuristics
python3 rules_cogling.py  --join <lobby-game-id>              # join a lobby instead
python3 -m unittest -v                                    # tests
```

Write your own cogling:

```python
from cogling_client import Cogling, run

class MyCogling(Cogling):
    def decide(self, decision):
        return decision.legal_actions[0]   # observation: decision.observation

run(MyCogling(), "ws://127.0.0.1:7878/ws", name="my-cogling", version="1.0", game="tictactoe")
```
