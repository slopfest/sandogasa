// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where sandogasa-report's forge tokens are kept: the desktop keyring
//! when there is one, one entry per forge instance, named for the
//! service it unlocks (#79), else the user overlay. The entries
//! themselves are named by each backend's `secret()`. The tool's own
//! tests get a keyring in memory, so `cargo test` never reads or
//! writes the user's.

sandogasa_config::keyring_fn!();
