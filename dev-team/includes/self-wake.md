## Waking yourself (Claude members)

Nothing types into your terminal to tell you a record has arrived. You wake yourself with a background wait:

1. **At the start of every turn,** if no wait of yours is running, start one with your Bash tool in **background** mode (not in the foreground, which blocks the turn):
   `"$BORGCHART_BIN" inbox wait --after <ID> --timeout 600 --json`
   where `<ID>` is the highest record ID you have listed so far (IDs are `m<number>`: compare the numbers, so `m10` is after `m9`). If you have never listed a record, leave out `--after`.
2. **When it returns,** list your inbox (`"$BORGCHART_BIN" inbox --json`, adding `--after <ID>` for further pages while `next` isn't null), act on each record, and ack what you handled (`"$BORGCHART_BIN" msg ack <id>…`). Then start the next wait, with the new highest ID.
3. **A timeout** (exit 9, outcome `timeout`) means nothing new: start the same wait again.
4. **`fenced`** (exit 8, outcome `fenced`) means your own session is no longer current: you were stopped or restarted, or the team was. Stop the wait loop: start no more waits, don't retry, and never set or change the `BORGCHART_*` variables. Say so where you can: the lead tells the owner on GitHub (see "Talking to the owner"); any other member does nothing more, since the lead restarts it.
   The one exception: a `fenced` whose message ends `list the inbox again` (the team was restarted under your `--after` cursor) is worth **one** `"$BORGCHART_BIN" inbox --json` without `--after`. If that works, carry on from the highest ID it shows; if it is `fenced` too, stop as above.

Pass a correct `--after` every time. A stale one returns at once for records you have already seen, and you will wake in a loop. The wait only returns for *unacknowledged* records, so ack what you have handled.

(This changes when brg's background-safe wait lands; this section will be updated with it.)
