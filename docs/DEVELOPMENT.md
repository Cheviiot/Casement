# Разработка

## Окружение

Сборка идёт на Fedora 44 (в CI — контейнер `fedora:44`, локально — Distrobox
`oriel-dev`). Rust — stable из rustup.

```sh
sudo dnf install gcc pkgconf-pkg-config gtk4-devel libadwaita-devel \
    gstreamer1-devel gstreamer1-plugins-base-devel gstreamer1-plugins-bad-free-devel \
    binutils
rustup component add clippy rustfmt
```

Для кросс-сборки под Windows ещё:

```sh
sudo dnf install mingw64-gcc mingw64-gtk4 mingw64-appstream mingw64-fribidi \
    mingw64-hicolor-icon-theme mingw64-opus mingw64-gstreamer1-plugins-base \
    mingw64-gstreamer1-plugins-good mingw64-gstreamer1-plugins-bad-free \
    mingw32-nsis sassc meson ninja-build curl zip glib2-devel
rustup target add x86_64-pc-windows-gnu
scripts/build-libadwaita-mingw          # Libadwaita для mingw, один раз
```

На машине автора всё запускается в контейнере: `scripts/dev/in-box КОМАНДА` выполняет
команду в `oriel-dev` с низким приоритетом и cargo из `~/.local/share/toolchains`
(`scripts/cargo`).

## Сборка и проверки

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
scripts/check-glibc target/release/casement   # не новее glibc 2.38 (ALT p11)
```

Программы линкуются в Fedora, а работать должны и на ALT p11 с glibc 2.38:
`scripts/check-glibc` следит за версиями символов, а
`crates/casement-net/src/glibc_compat.rs` закрывает `pidfd_spawnp` и `pidfd_getpid`.

Тесты сети идут через loopback и сторонних машин не требуют.

## Пакеты

| Команда | Результат в `dist/` |
|---|---|
| `scripts/package-linux` | `casement-ВЕРСИЯ-linux-x86_64.tar.gz` с `install.sh` |
| `scripts/package-alt` | `casement-ВЕРСИЯ-alt1.x86_64.rpm` (в контейнере ALT p11) |
| `scripts/package-fedora` | `casement-ВЕРСИЯ-1.fc44.x86_64.rpm` (в контейнере Fedora 44) |
| `scripts/package-deb` | `casement_ВЕРСИЯ_amd64.deb` (в контейнере Debian 13) |
| `scripts/package-windows` | `casement-ВЕРСИЯ-windows-x86_64-setup.exe` (NSIS) и `-portable.zip` |
| `flatpak-builder --user --install build-flatpak packaging/flatpak/io.github.cheviiot.Casement.yml` | Flatpak |
| `scripts/install-user` | ставит текущую сборку себе в `~/.local` |

Пакеты Linux берут готовую `cargo build --release`: программы одни и те же, а
зависимости каждый пакет вычисляет на своём дистрибутиве. `package-windows` сам
собирает `x86_64-pc-windows-gnu`, кладёт рядом все DLL, плагины GStreamer, схемы и
значки и делает из этого установщик (`packaging/windows/casement.nsi`) и архив.
Значок встраивается в `.exe` при сборке (`packaging/windows/icon.rs`).

## CI и релизы

`.github/workflows/build.yml` на каждый push и pull request проверяет форматирование,
clippy и тесты, собирает архив Linux, пакеты ALT, Fedora и Debian, установщик Windows и
Flatpak. Тег `vX.Y.Z` (совпадающий с версией в `Cargo.toml`) после тех же шагов
публикует релиз со всеми пакетами, `install.sh` и `SHA256SUMS`:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

## Проверки без рабочего стола

- `scripts/dev/ui-check OUT.png [СТРАНИЦА]` — снимок окна под headless Broadway, ничего
  не появляется на экране. Список страниц — в начале скрипта.
- `casement-cli listen` на одной машине и `casement-cli connect АДРЕС --record a.h264
  --wiggle` на другой: вход, запись экрана, движение указателя. `--latency N` меряет
  задержку от движения до кадра, `--audio N` — звук, `--clipboard` — буфер обмена.
- `casement-cli decode-bench a.h264` сравнивает способы декодирования на этой машине.
- На Windows процессы из SSH живут в сеансе 0 без рабочего стола: хост для проверки
  запускается задачей планировщика в интерактивном сеансе пользователя.

## Что дальше

- Передача кадра в окно без копирования (DMABuf): нужна проверка на живом Wayland.
- Курсор метаданными, а не в картинке, — меньше задержка указателя.
- Экран входа в систему: системная служба на Windows, отдельный путь на Linux.
- Пакет ALT в репозиторий дистрибутива (gear/hasher), подпись Windows-программ.
