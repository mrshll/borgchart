## Waking yourself (Claude members)

Nothing types into your terminal to tell you a record has arrived. You wake yourself with a background wait:

1. **At the start of every turn,** if no wait of yours is running, start one with your Bash tool in **background** mode (not in the foreground, which blocks the turn):
   `"$BORGCHART_BIN" inbox wait --after <ID> --timeout 600 --json`
   where `<ID>` is the highest record ID you have listed so far (IDs are `m<number>`: compare the numbers, so `m10` is after `m9`). If you have never listed a record, leave out `--after`.
2. **When it returns,** list your inbox (`"$BORGCHART_BIN" inbox --json`, adding `--after <ID>` for further pages while `next` isn't null), act on each record, and ack what you handled (`"$BORGCHART_BIN" msg ack <id>…`). Then start the next wait, with the new highest ID.
3. **A timeout** (exit 9, outcome `timeout`) means nothing new: start the same wait again.
4. **`fenced`** (exit 8) means the team was restarted and IDs started over: list your inbox again without `--after`, then wait from the highest ID it shows.

Pass a correct `--after` every time. A stale one returns at once for records you have already seen, and you will wake in a loop. The wait only returns for *unacknowledged* records, so ack what you have handled.

(This changes when brg's background-safe wait lands; this section will be updated with it.)
