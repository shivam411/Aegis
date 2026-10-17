# Releasing Aegis

Releases are built and published by
[`.github/workflows/release.yml`](../.github/workflows/release.yml) when a
`v*` tag is pushed (or a release is published). Besides the archives
(`aegis-<os>-<arch>.tar.gz`, `aegis-windows-amd64.zip`) every release has:

- `SHA256SUMS`: the SHA-256 of every archive.
- `SHA256SUMS.minisig`: a [minisign](https://jedisct1.github.io/minisign/)
  signature of `SHA256SUMS` made with the **release signing key**. Its
  trusted comment is `aegis-release:<tag> file:SHA256SUMS`, which binds the
  signature to that release.

Every installer checks the signature before it trusts `SHA256SUMS`: the
dashboard's "Upgrade now", the root update helper, `aegis-daemon
self-update`, `aegis upgrade` and `install.sh`. The public keys they accept
are in [`core/update/keys/release.pub`](../core/update/keys/release.pub)
(compiled into Aegis) and in `RELEASE_KEYS` in [`install.sh`](../install.sh)
(a test keeps the two lists equal). See [`security.md`](security.md#self-update)
for the trust model.

> **Until the one-time setup below is done**, `release.pub` holds a
> placeholder with no key: the release workflow fails before building
> (nothing is published), and Aegis builds refuse to install any release
> unless an administrator runs `aegis-daemon self-update --allow-unsigned`
> (or `install.sh --allow-unsigned`).

## One-time setup

Do this on a machine you trust, ideally offline, with
[minisign](https://jedisct1.github.io/minisign/) installed (`apt install
minisign`, `brew install minisign`, `dnf install minisign` from EPEL).

1. **Generate the key pair** (it asks for a password; use a strong one):

   ```sh
   minisign -G -p aegis-release.pub -s aegis-release.key
   ```

   `aegis-release.key` is the encrypted secret key. Keep it, and its
   password, somewhere safe and backed up (a password manager, an offline
   copy); never commit it. `aegis-release.pub` is the public key.

2. **Commit the public key.** `aegis-release.pub` has two lines; the second
   (starting with `RW`) is the key. Add it:
   - as a new line in `core/update/keys/release.pub` (you may delete the
     "PLACEHOLDER" paragraph);
   - inside `RELEASE_KEYS=( ... )` in `install.sh`, quoted:
     ```sh
     RELEASE_KEYS=(
         "RWQ...your key..."
     )
     ```
   Run `cargo test -p aegis-update` (it checks that both lists match and
   parse) and merge this to `main` through a pull request. Builds from that
   commit on trust the key. The key ID (what the dashboard and logs show) is
   printed by `minisign -G` and in the first line of the `.pub` file.

3. **Add the secrets** to the repository (Settings → Secrets and variables →
   Actions), or to an environment named `release` (Settings → Environments;
   the release jobs use that environment, so you can also require a
   reviewer's approval there before anything is signed):

   | Secret | Value |
   | :--- | :--- |
   | `MINISIGN_SECRET_KEY` | The whole content of `aegis-release.key` (both lines) |
   | `MINISIGN_PASSWORD` | The key's password (leave unset for a key made with `minisign -G -W`, without a password) |

   With the GitHub CLI:

   ```sh
   gh secret set MINISIGN_SECRET_KEY < aegis-release.key
   gh secret set MINISIGN_PASSWORD        # prompts for the value
   # or, for the environment: add --env release
   ```

4. **Release as usual** (`git tag v0.x.y && git push origin v0.x.y`). The
   `Release signing key is set up` job checks the secret and the committed
   key first; the release job signs `SHA256SUMS`, verifies the signature with
   the committed key (with minisign, and with install.sh's own check) and only
   then publishes. If any of that fails, nothing is published; fix it and
   re-run the workflow.

Installs of releases from before signing support don't check signatures at
all; they upgrade as before, and the version they upgrade to checks every
later release. Builds made while `release.pub` held the placeholder (e.g.
from source) have no key and can't verify anything: upgrade those once by
hand, by re-running `install.sh` (from `main`, which contains the key), or
on servers with `sudo aegis-daemon self-update --allow-unsigned` after
checking the release yourself:

```sh
minisign -Vm SHA256SUMS -x SHA256SUMS.minisig -P RWQ...the key...
sha256sum -c --ignore-missing SHA256SUMS
```

## Rotating the key

A build only accepts signatures from the keys compiled into it, so a new key
must ship in a release signed by the old one before releases can be signed
with it:

1. Generate the new pair (`minisign -G -p aegis-release-2.pub -s aegis-release-2.key`).
2. Add the new public key to `core/update/keys/release.pub` and `install.sh`,
   **keeping the old one**, and merge.
3. Publish a release (still signed with the old key, which is in the
   secrets). Installs that upgrade to it now trust both keys.
4. Replace `MINISIGN_SECRET_KEY` / `MINISIGN_PASSWORD` with the new key.
   Releases are now signed with the new key; builds from step 2 on accept it.
5. Once you no longer expect anyone to upgrade directly from a build older
   than step 3 (say, a few releases later), remove the old key from both
   files.

An install that skips the transition release (it still trusts only the old
key) refuses releases signed with the new one ("signed by key X, which this
build does not trust"); its administrator upgrades it once by hand as
described above.

## If the secret key leaks

Anyone with the key and its password can sign `SHA256SUMS` files that every
existing install accepts. They still need a way to deliver them: publishing a
release in this repository (or a fork that an install is configured to use
with `[update] repo`), or breaking TLS to GitHub. So treat a leak as urgent:

1. Generate a new key pair. Replace the old key with the new one in
   `release.pub` and `install.sh` (do **not** keep the leaked key), update the
   secrets, and publish a release signed with the new key.
2. Publish a security advisory (see [SECURITY.md](../SECURITY.md)) naming the
   leaked key ID, and tell administrators to upgrade **by hand** with the
   current `install.sh` (or to verify the release with the new public key
   before `self-update --allow-unsigned`). There is no revocation list:
   existing installs keep trusting the leaked key until they run a build
   without it, and they can't verify the new key's releases until then.
3. Review recent releases and the repository's access (tokens, collaborators,
   workflow changes) for misuse.

If only the GitHub repository or a workflow token is compromised (not the
key), installs refuse anything the attacker publishes, because it can't be
signed; check that the attacker didn't change `release.pub`/`install.sh` on
`main`, since new builds and new `install.sh` runs trust what is committed
there.
