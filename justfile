version := `grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/'`

[doc("Show current version")]
current:
    @echo "{{version}}"

[doc("Bump version: just bump 0.2.0")]
bump new_version:
    sed -i '0,/^version = ".*"/s//version = "{{new_version}}"/' Cargo.toml
    cargo update --workspace
    @echo "Bumped to {{new_version}}"

[doc("Run all tests")]
test:
    cargo test --workspace

[doc("Build release binary")]
build:
    cargo build --release

[doc("Build and install locally")]
install: build
    sudo install -Dm755 target/release/zestview /usr/bin/zestview
    sudo install -Dm644 zestview.desktop /usr/share/applications/zestview.desktop
    -sudo update-desktop-database -q /usr/share/applications
    @echo "Installed"

[doc("Make zestview the default app for every image type it supports")]
set-default:
    #!/usr/bin/env bash
    set -euo pipefail
    mimes=$(grep '^MimeType=' zestview.desktop | cut -d= -f2 | tr ';' ' ')
    xdg-mime default zestview.desktop $mimes
    echo "zestview is now the default for: $mimes"

[doc("Remove the installed binary and desktop entry")]
uninstall:
    sudo rm -f /usr/bin/zestview /usr/share/applications/zestview.desktop
    -sudo update-desktop-database -q /usr/share/applications
    @echo "Uninstalled"

[doc("Bump, commit, tag, and push. Optional message: just release 0.2.0 \"animated images\"")]
release new_version msg="":
    just bump {{new_version}}
    git add Cargo.toml Cargo.lock
    if git diff --cached --quiet; then \
        echo "Version bump already committed; continuing release"; \
    else \
        git commit -m "v{{new_version}}{{ if msg != "" { ": " + msg } else { "" } }}"; \
    fi
    git tag -a "v{{new_version}}" -m "v{{new_version}}{{ if msg != "" { ": " + msg } else { "" } }}"
    git push && git push --tags
    gh release create "v{{new_version}}" --generate-notes
