PREFIX ?= /usr/local

.PHONY: build windows-setup install uninstall
build:
	cargo build --release --locked

windows-setup:
	python3 scripts/build_windows.py

install:
	install -Dm755 target/release/discord-parcel "$(DESTDIR)$(PREFIX)/bin/discord-parcel"
	install -Dm644 data/dev.akaduy.DiscordParcel.desktop "$(DESTDIR)$(PREFIX)/share/applications/dev.akaduy.DiscordParcel.desktop"
	install -Dm644 data/dev.akaduy.DiscordParcel.svg "$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/dev.akaduy.DiscordParcel.svg"
	install -Dm644 data/dev.akaduy.DiscordParcel.xml "$(DESTDIR)$(PREFIX)/share/mime/packages/dev.akaduy.DiscordParcel.xml"
	update-mime-database "$(DESTDIR)$(PREFIX)/share/mime"
	-update-desktop-database "$(DESTDIR)$(PREFIX)/share/applications"
	-gtk-update-icon-cache -f -t "$(DESTDIR)$(PREFIX)/share/icons/hicolor"

uninstall:
	rm -f "$(DESTDIR)$(PREFIX)/bin/discord-parcel" "$(DESTDIR)$(PREFIX)/share/applications/dev.akaduy.DiscordParcel.desktop" "$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/dev.akaduy.DiscordParcel.svg" "$(DESTDIR)$(PREFIX)/share/mime/packages/dev.akaduy.DiscordParcel.xml"
	update-mime-database "$(DESTDIR)$(PREFIX)/share/mime"
