# NBX tools

A Simple Bot-like interface I built for helping sell Ethereum gradually on nbx.com.

## `nbx-api`

A rust API implementing the parts of the NBX API I needed. The API is not that great. I would rethink a lot of things if i was to aim to release this as something i'd expect others to use.

## `nbx-tui`

A TUI app to display the information I was interested in during the selling period.

#### Usage

- Shift + page up/down: increase or decrease the sell limit
- Shift + B: reset the sell timer
- i: toggle ignoring the current coinbase price when deciding the sell limit
