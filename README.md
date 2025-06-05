# sparse

sparse is tui client for [matrix](https://matrix.org).

Its main features include:
 - a text-communication-focused interface
 - configurable vim-like modes and multi-key keybindings
 - custom scriptable functions via lua configuration file
 - vim-like message composition

## Configuration

sparse is configured via a lua file which is evaluated at startup.
This file includes one-time configuration like `host` and `user`, but also functions which can be bound to key sequences and will thus be evaluated dynamically.

`sample_config.lua` contains a fairly minimal example configuration.
This file (when specified as a command line argument or copied to `<config_dir>/sparse/config.lua`) is loaded in addition to the static `src/base_config.lua` (compiled into the binary).
Have a look at the latter for default key bindings and modes.

## Building

sparse is written in rust and as such can be built using `cargo build --release`.
The resulting binary can be found in the target directory.

## State

I consider sparse feature complete and have been using it daily for all my non-phone matrix communication since 2022.
As such I currently don't plan on adding functionality, but will maintain the software.

## License

`sparse` is released under the MIT license.
