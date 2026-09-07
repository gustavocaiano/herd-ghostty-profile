# Updater diário

O updater compara a versão de `/Applications/Ghostty.app` com a versão de cada app: `~/Applications/Herdr.app` (Local) e todas as apps por desktop configuradas no `desktops.toml` (ex.: `~/Applications/Devbox.app` para o `devbox`).

Cada app é processada de forma independente: se a versão for diferente, a app é recriada com `scripts/create-herdr-app.sh` (com os `APP_NAME`/`BUNDLE_ID`/`TARGET_APP` correspondentes); se já estiver atualizada, fica como está.

Por segurança, cada app que estiver aberta é skipada (registada no log, sem falhar a execução) e tenta-se novamente na próxima execução. No fim, o updater escreve a versão fonte em `~/.config/herd/state/ghostty-version.txt` e, se o Desktop Switcher estiver instalado, corre o respetivo `--check` só-de-leitura.

## Instalar LaunchAgent

```bash
mkdir -p "$HOME/Library/LaunchAgents"
mkdir -p "$HOME/.config/herd/logs" "$HOME/.config/herd/state"
cp ~/.config/herd/launchd/com.gustavocaiano.herdr-app-updater.plist "$HOME/Library/LaunchAgents/"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.gustavocaiano.herdr-app-updater.plist"
```

## Correr agora manualmente

```bash
launchctl kickstart -k "gui/$(id -u)/com.gustavocaiano.herdr-app-updater"
```

## Desinstalar

```bash
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.gustavocaiano.herdr-app-updater.plist"
rm "$HOME/Library/LaunchAgents/com.gustavocaiano.herdr-app-updater.plist"
```

## Logs e estado

```text
~/.config/herd/logs/updater.out.log
~/.config/herd/logs/updater.err.log
~/.config/herd/state/ghostty-version.txt
```
