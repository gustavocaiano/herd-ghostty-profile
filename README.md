# Herd Ghostty profile

Configuração para usar **Herdr dentro do Ghostty** com atalhos estilo cmux/macOS, e opcionalmente criar uma mini app `Herdr.app` separada para Dock/shortcuts.

## Quick activation

1. Criar `Herdr.app`:

   ```bash
   ~/.config/herd/scripts/create-herdr-app.sh
   ```

2. Abrir a app:

   ```bash
   open "$HOME/Applications/Herdr.app"
   ```

3. Apontar o teu shortcut, por exemplo `Ctrl+3`, para:

   ```text
   ~/Applications/Herdr.app
   ```

4. Opcional: instalar o updater diário:

   ```bash
   mkdir -p "$HOME/Library/LaunchAgents"
   mkdir -p "$HOME/.config/herd/logs" "$HOME/.config/herd/state"
   cp ~/.config/herd/launchd/com.gustavocaiano.herdr-app-updater.plist "$HOME/Library/LaunchAgents/"
   launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.gustavocaiano.herdr-app-updater.plist"
   ```

Nota: como `Herdr.app` é uma cópia local modificada e assinada ad-hoc, o macOS pode mostrar um aviso na primeira abertura. Para uso local isto é esperado; se bloquear, abre uma vez com botão direito → **Open** ou permite em **Privacy & Security**.

## O que está neste repo

```text
assets/herdr.icns                         ícone arredondado para Herdr.app
ghostty-herdr.conf                        snapshot do profile Ghostty para Herdr
herdr-config.toml                         snapshot das keybindings Herdr
herd.zsh                                  função zsh simples
desktops.toml                             endpoints declarativos do Desktop Switcher (sem credenciais)
plugins/herdr-desktop-switcher/           plugin Desktop Switcher (binário em bin/, gitignored)
scripts/create-herdr-app.sh               cria/recria ~/Applications/Herdr.app e outras apps Herdr
scripts/update-herdr-app.sh               atualiza Herdr.app e as apps por desktop quando Ghostty muda
scripts/sync-desktop-apps.sh              cria as apps por desktop remoto em falta (ex.: Devbox.app)
scripts/install-desktop-switcher.sh       instalador e --check do Desktop Switcher
scripts/herdr-desktop-command             command shim genérico do Desktop Switcher
scripts/herdr-desktop-launch.swift        launcher macOS do Desktop Switcher
scripts/sync-configs.sh                   instala snapshots de config nos caminhos ativos
launchd/com.gustavocaiano.herdr-app-updater.plist
docs/setup.md                             setup completo
docs/herdr-app.md                         detalhes da mini app
docs/updater.md                           updater diário via launchd
docs/desktop-switcher.md                  detalhes do Desktop Switcher
docs/troubleshooting.md                   notas e problemas conhecidos
```

## Desktop Switcher (multi-remote)

Para lançar vários clientes Herdr (Local, `devbox`, futuros remotos), cada um na
sua própria app do Dock — `Herdr.app` para o Local, `Devbox.app` para o
`devbox` — usar o instalador do plugin:

```bash
~/.config/herd/scripts/install-desktop-switcher.sh
```

Verificação só-de-leitura:

```bash
~/.config/herd/scripts/install-desktop-switcher.sh --check
```

Atalho do picker (ativado pelo instalador via `scripts/sync-configs.sh`):

- `Cmd+Shift+K` → abre o Desktop Switcher

O instalador cria as apps por desktop em falta via
`scripts/sync-desktop-apps.sh` (nunca recria as que já existem; a cópia usa
clonefile APFS quando disponível). Detalhes em `docs/desktop-switcher.md`. O
instalador não escreve credenciais nem configuração SSH; o target remoto `dev`
tem de estar configurado em `~/.ssh/config` antes de QA remoto.

## Caminhos ativos

- Ghostty profile usado pelo Herdr: `~/.config/ghostty/herdr`
- Config Herdr: `~/.config/herdr/config.toml`
- Mini app opcional: `~/Applications/Herdr.app`
- Apps por desktop (Desktop Switcher): `~/Applications/Herdr.app` (Local), `~/Applications/Devbox.app` (`devbox`), etc.

## Setup rápido: função `herd`

Adiciona isto ao `~/.zshrc`:

```zsh
herd() {
  local app="$HOME/Applications/Herdr.app"
  local bundle_id="com.gustavocaiano.herdr"

  if pgrep -f "$app/Contents/MacOS/(herdr-launcher|ghostty-bin)" >/dev/null 2>&1; then
    osascript -e "tell application id \"$bundle_id\" to activate" >/dev/null 2>&1 || open "$app"
  else
    open "$app"
  fi
}
```

Depois:

```zsh
source ~/.zshrc
herd
```

## Setup com app separada: `Herdr.app`

Para ter nome/ícone separados no Dock e apontar shortcuts diretamente para a app:

```bash
~/.config/herd/scripts/create-herdr-app.sh
```

Depois abre:

```bash
open "$HOME/Applications/Herdr.app"
```

Esta app é uma cópia local de `Ghostty.app` com:

- bundle name/display name: `Herdr`
- bundle id: `com.gustavocaiano.herdr`
- ícone: `assets/herdr.icns` via `CFBundleIconFile = Herdr` e sem `CFBundleIconName` herdado da Ghostty
- launcher nativo que inicia Ghostty com `--config-file ~/.config/ghostty/herdr --command ~/.local/bin/herdr`
- assinatura local ad-hoc (`codesign --sign -`)

## Atalhos

No profile Herdr:

- `Cmd+N` novo workspace Herdr
- `Cmd+T` novo tab Herdr
- `Cmd+D` split right
- `Cmd+Shift+D` split down
- `Cmd+W` fechar pane
- `Ctrl+Cmd+↑/↓` workspace anterior/seguinte
- `Ctrl+Cmd+←/→` tab anterior/seguinte
- `Ctrl+arrows` foco entre panes

## Updater diário opcional

Instala o LaunchAgent:

```bash
mkdir -p "$HOME/Library/LaunchAgents"
mkdir -p "$HOME/.config/herd/logs" "$HOME/.config/herd/state"
cp ~/.config/herd/launchd/com.gustavocaiano.herdr-app-updater.plist "$HOME/Library/LaunchAgents/"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.gustavocaiano.herdr-app-updater.plist"
```

Ele corre diariamente e recria `Herdr.app` e as apps por desktop (ex.: `Devbox.app`) quando a versão de `/Applications/Ghostty.app` muda. As apps que estiverem abertas são skippadas por segurança.

## Notas importantes

- `macos-icon` no Ghostty é app-wide para o bundle, não por janela/profile.
- Para ícone separado só no Herdr, usa `Herdr.app` separada.
- A app copiada é assinada localmente, não com a assinatura oficial da Ghostty.
- Isto é adequado para uso local; para distribuição pública seria necessário Developer ID/notarização.

Ver detalhes em `docs/setup.md`.
