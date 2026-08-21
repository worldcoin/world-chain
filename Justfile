set positional-arguments := true
set dotenv-load := true

import 'justfiles/rust.just'
import 'justfiles/contracts.just'
import 'justfiles/devnet.just'
import 'justfiles/proof.just'

# default recipe to display help information
default:
    @just --list
