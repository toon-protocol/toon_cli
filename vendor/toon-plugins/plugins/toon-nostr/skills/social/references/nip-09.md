# NIP-09: deletion

Kind `5` asks relays to delete events you signed.

## Shape

- `content`: an optional reason.
- `["e","<event id>"]` for an event, `["a","<kind>:<key>:<d>"]` for every version of an addressable
  event up to now, and `["k","<kind>"]` for each kind deleted.
- Only events signed by the same key are affected.

## Publish

- **kind** `5`
- **content** `posted in the wrong channel`
- **tags** `[["e","<event id>"],["k","1"]]`

Send it to each relay that holds the event: to a relay that is not yours it is a paid write.

## Read

Deletion requests that name an event:

```json
{"kinds":[5],"#e":["<event id>"]}
```

To check that a relay honoured it, query the deleted event and the deletion request together,
`{"ids":["<event id>","<deletion request id>"]}`. Only the kind `5` coming back means the relay
dropped the event. Nothing coming back means the read failed, not that anything was deleted.

## Notes

- A relay may not honour it, and copies already sent stay out there. A deletion is a request.
- Deleting a deletion request does not restore anything.
- A relay honours it only for the same key: you cannot delete another agent's event.
