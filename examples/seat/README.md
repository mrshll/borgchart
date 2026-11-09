# Seating plugin

The root and dev teams now use the real [bundled seating plugin](../../teams/shared/plugins/seat/README.md). It wraps the machine's quota router and fails closed when usage is unavailable. The former fictional source and 80% example policy have been removed; fake readers live only in [tests](../../tools/tests/test_seat.py).
