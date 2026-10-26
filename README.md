<p align="center">
<img alt="Carcinization OS" width="200" src="logos/carcinization/logo.png">
</p>

<h1 align="center">Carcinization OS</h1>

This repository is the **Build System** for Carcinization OS.

Carcinization is under active development by a vibrant community, you can see the key links below:

- [The **main website** for Carcinization OS](https://www.redox-os.org).
- [The Carcinization Book](https://doc.redox-os.org/book/) and [Build Instructions](https://doc.redox-os.org/book/podman-build.html).
- [Carcinization Chat and Support](https://matrix.to/#/#redox-join:matrix.org).
- [Patreon](https://www.patreon.com/redox_os), [Donate](https://redox-os.org/donate/) and [Merch](https://redox-os.creator-spring.com/).
- Scroll down for a list of key Carcinization components and their repositories.

[Carcinization](https://www.redox-os.org) is an open-source operating system written in Rust, a language with focus on safety, efficiency and high performance. Carcinization uses a microkernel architecture, and aims to be reliable, secure, usable, correct, and free. Carcinization is inspired by previous operating systems, such as seL4, MINIX, Plan 9, Linux and BSD.

Carcinization _is not_ just a kernel, it's a **full-featured operating system**, providing components (file system, display server, core utilities, etc.) that together make up a functional and convenient operating system. Carcinization uses the COSMIC desktop apps, and provides source code compatibility with many Rust, Linux and BSD programs.

[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)

## More Links

- [Book](https://doc.redox-os.org/book/)
- [Contribute](CONTRIBUTING.md)
- [Hardware Compatibility](https://doc.redox-os.org/book/hardware-support.html)
- Run Carcinization in a [Virtual Machine](https://doc.redox-os.org/book/running-vm.html) or on [Real Hardware](https://doc.redox-os.org/book/real-hardware.html)
- [Trying Out Carcinization](https://doc.redox-os.org/book/trying-out-redox.html)
- [Building Carcinization](https://doc.redox-os.org/book/podman-build.html)
- [Build System Documentation](https://doc.redox-os.org/book/build-system-reference.html)
- [Developer FAQ](https://doc.redox-os.org/book/developer-faq.html)
- [Chat/Discussions/Help](https://doc.redox-os.org/book/chat.html)

## Ecosystem

Some of the key repositories on the Carcinization GitLab:

| Essential Repositories                                                               | Maintainer
|-------------------------------------------------------------------------------------------------------------|---------------------------
| [Kernel](https://gitlab.redox-os.org/redox-os/kernel)                                                       | **@jackpot51**
| [Base (essential system components and drivers)](https://gitlab.redox-os.org/redox-os/base)                             | **@jackpot51**
| [CarcinizationFS (default filesystem)](https://gitlab.redox-os.org/redox-os/redoxfs)                                | **@jackpot51**
| [relibc (C POSIX library written in Rust)](https://gitlab.redox-os.org/redox-os/relibc)                     | **@jackpot51**
| [Ion (defauilt shell)](https://gitlab.redox-os.org/redox-os/ion)                                            | **@jackpot51**
| [Termion (terminal library)](https://gitlab.redox-os.org/redox-os/termion)                                  | **@jackpot51**
| [pkgutils (current package manager)](https://gitlab.redox-os.org/redox-os/pkgutils)                         | **@jackpot51**
| [Orbital (display server and window manager)](https://gitlab.redox-os.org/redox-os/orbital)                 | **@jackpot51**
| This repo - the root of the Build System                                                                    | **@jackpot51** **@hatred_45**
| [Carcinizer (tool for easy Carcinization development on Linux)](https://gitlab.redox-os.org/redox-os/redoxer)          | **@jackpot51**
| [The Carcinization Book](https://gitlab.redox-os.org/redox-os/book)                                                 | **@jackpot51** **@hatred_45**
| [Website](https://gitlab.redox-os.org/redox-os/website)                                                     | **@jackpot51** **@hatred_45**

## Package management

Carcinization ships two package managers, and you can use either one.

**`pkg`** is the native package manager. It works with `pkgar`, the signed
package format used by the repository, and it is what the build system itself
uses to publish packages.

```
pkg install curl
pkg update
pkg search editor
pkg info curl
pkg list
```

**`apt`, `apt-get` and `apt-cache`** are Debian-compatible frontends for `pkg`.
They exist so that the command line you already know keeps working:

```
apt install curl
apt-get install curl        # never prompts, for scripts
apt-cache search editor
apt remove curl
apt update
```

They are thin translators rather than a second implementation: `pkg` stays the
single source of truth about what is installed, so the two frontends can never
disagree. A few Debian-only operations are refused rather than silently
approximated — `apt source` and `apt --simulate` have no equivalent in the
`pkgar` backend and say so.

> **Note on Debian packages.** `apt install <name>` installs packages built for
> Carcinization, not `.deb` files from Debian or Ubuntu. Carcinization uses
> [relibc](https://gitlab.redox-os.org/redox-os/relibc) rather than glibc, so
> binaries from a Debian archive are linked against a different C library and
> cannot run here. What Debian is genuinely useful for is upstream *source*
> code: the recipes in this repository are largely upstream tarballs plus the
> patches needed to build them against relibc.

### Installing `.deb` packages

Debian packages are supported as a second source, on top of the native
`pkgar` format:

```
apt install ./package.deb      # install a .deb file you already have
apt install curl               # fetch from the configured Debian mirrors
apt remove curl                # keep the configuration files
apt purge curl                 # remove the configuration files too
apt list                       # show what came from .deb packages
apt-cache policy curl          # what the mirror offers
```

`apt update` refreshes the package index from the mirrors listed in
`/etc/apt/sources.list`, which uses Debian's own format:

```
deb http://deb.debian.org/debian bookworm main
```

Downloads are verified against the SHA256 checksum from the index. Installed
packages are recorded in a dpkg-compatible database under
`/var/lib/dpkg`, so `dpkg -l` style inspection keeps working.

Packages built for Carcinization always win: `apt install` tries the native
repository first, and only falls back to a Debian archive when the package is
not available there.

> **The glibc caveat.** A `.deb` from Debian carries binaries linked against
> glibc. Carcinization uses relibc, so those binaries install but will not
> run. `apt` detects this and says so after installing, listing the affected
> files. To get working binaries you need the Debian *source* package rebuilt
> against relibc.

### Building Debian source packages

The build system can take its sources from a Debian source package, which is
how the same upstream software ends up running here with relibc instead of
glibc:

```toml
[source]
dsc = "https://deb.debian.org/debian/pool/main/h/hello/hello_2.10-3.dsc"
```

The `.dsc` file and the tarballs it references are downloaded and verified
against the checksums it carries. The upstream sources are laid out, the
Debian packaging is unpacked over them and the quilt series is applied. The
result is an ordinary source tree, built with the usual templates — and
therefore linked against relibc, so it runs.

See [CONTRIBUTING.md](CONTRIBUTING.md#debian-source-packages) for the details
and `recipes/wip/deb-hello` for a working example.

## What it looks like

See [Carcinization in Action](https://www.redox-os.org/screens/) for photos and videos.

<img alt="Carcinization" height="150" src="https://gitlab.redox-os.org/redox-os/website/-/raw/master/static/img/screenshot/orbital-visual.png">
<img alt="Carcinization" height="150" src="https://gitlab.redox-os.org/redox-os/website/-/raw/master/static/img/screenshot/cosmic-programs.png">
<img alt="Carcinization" height="150" src="https://gitlab.redox-os.org/redox-os/website/-/raw/master/static/img/screenshot/cosmic-term-screenfetch.png">

<img alt="Carcinization" height="150" src="https://gitlab.redox-os.org/redox-os/website/-/raw/master/static/img/screenshot/cosmic-edit-redox.png">
<img alt="Carcinization" height="150" src="https://gitlab.redox-os.org/redox-os/website/-/raw/master/static/img/screenshot/image-viewer.png">
<img alt="Carcinization" height="150" src="https://gitlab.redox-os.org/redox-os/assets/raw/master/screenshots/Boot.png">
