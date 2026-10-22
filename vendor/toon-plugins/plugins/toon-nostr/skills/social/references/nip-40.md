# NIP-40: expiration

An `expiration` tag asks relays to drop an event after a time.

## Shape

`["expiration","<unix time in seconds>"]` on any event. A relay should not serve the event after it,
and may delete it.

## Publish

- **kind** `1`
- **content** `meeting in this channel at noon`
- **tags** `[["expiration","1893456000"]]`

## Read

Nothing selects on expiry: query as usual. A relay that honours it simply returns nothing for an
expired event.

```json
{"kinds":[1],"authors":["<key>"],"limit":10}
```

## Notes

- Your own relay's setting decides: a TOON relay either honours expiry or ignores it, as its
  operator set it.
- The time is yours to compute. A relay may have already sent the event to others, who may keep it.
- Expiry is not secrecy: before the time, the event is public.
