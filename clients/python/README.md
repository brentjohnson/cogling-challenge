# Python client and sample bots

Standard library only (Python 3.9+). `ogap_client.py` includes a minimal `ws://`
WebSocket client plus a bot runner. Real bots can use any WebSocket library,
since the protocol is plain JSON (see `docs/protocol.md`).

```sh
python3 random_bot.py --game tictactoe --games 3          # any game, random legal moves
python3 rules_bot.py  --game connect4  --games 3          # tictactoe/connect4 heuristics
python3 rules_bot.py  --join <lobby-game-id>              # join a lobby instead
python3 -m unittest -v                                    # tests
```

Write your own bot:

```python
from ogap_client import Bot, run

class MyBot(Bot):
    def decide(self, decision):
        return decision.legal_actions[0]   # observation: decision.observation

run(MyBot(), "ws://127.0.0.1:7878/ws", name="my-bot", version="1.0", game="tictactoe")
```
