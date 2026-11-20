// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Print docs/tokens.md, the credentials page, from the table in
//! `sandogasa_config::credentials`: `make tokens`.

fn main() {
    print!("{}", sandogasa_config::credentials::markdown());
}
