# Setup completo

## 1. Aplicar configs ativas

Usar o script de sincronização, que instala os snapshots versionados nos caminhos ativos:

```bash
~/.config/herd/scripts/sync-configs.sh
```

O script é idempotente, não aceita argumentos e resolve o caminho do repositório em relação a si mesmo (funciona de qualquer diretório). Ele gerencia apenas estes dois arquivos:

- `~/.config/herd/herdr-config.toml` → `~/.config/herdr/config.toml` (modo 0644)
- `~/.config/herd/ghostty-herdr.conf` → `~/.config/ghostty/herdr` (modo 0644)

Ele cria apenas `~/.config/herdr` e `~/.config/ghostty` se ausentes, e recarrega o Herdr com `herdr server reload-config` quando o binário `herdr` está disponível. Se o reload falhar, o script reporta o erro em vez de ocultá-lo.

O script não toca em nenhum outro arquivo — incluindo plugins, sessões, logs, sockets e estado de runtime sob `~/.config/herdr` ou `~/.local/state/` — nem em nenhum arquivo fora dos dois caminhos gerenciados acima.

## 2. Escolher modo de lançamento

### Opção A — função zsh para `Herdr.app`

Adicionar ao `~/.zshrc`:

```zsh
source "$HOME/.config/herd/herd.zsh"
```

Usar:

```zsh
herd
```

Esta função usa `Herdr.app`, portanto requer criar a app uma vez com `scripts/create-herdr-app.sh`.

### Opção B — abrir a mini app `Herdr.app` diretamente

Criar app separada:

```bash
~/.config/herd/scripts/create-herdr-app.sh
```

Abrir:

```bash
open "$HOME/Applications/Herdr.app"
```

Esta é a opção recomendada para shortcuts como `Ctrl+3`, porque macOS vê `Herdr.app` como app separada.

## 3. Atualizar quando Ghostty atualizar

Manual:

```bash
~/.config/herd/scripts/update-herdr-app.sh
```

Automático diário: ver `docs/updater.md`.

## 4. Desktop Switcher (multi-remote)

Para lançar vários clientes Herdr (Local, `devbox`, futuros remotos), cada um na
sua própria app do Dock (`Herdr.app` para o Local, `Devbox.app` para o
`devbox`), usar o instalador do plugin:

```bash
~/.config/herd/scripts/install-desktop-switcher.sh
```

Verificação só-de-leitura (sem builds nem escrita):

```bash
~/.config/herd/scripts/install-desktop-switcher.sh --check
```

Detalhes completos em `docs/desktop-switcher.md`. O instalador gere estes
artefatos:

- `plugins/herdr-desktop-switcher/` — plugin Rust; binário instalado em
  `plugins/herdr-desktop-switcher/bin/herdr-desktop-switcher`
- `scripts/herdr-desktop-launch.swift` — launcher macOS, compilado para
  `~/.local/bin/herdr-desktop-launch`
- `scripts/herdr-desktop-command` — command shim genérico
- `desktops.toml` — endpoints declarativos (sem credenciais)
- `scripts/install-desktop-switcher.sh` — instalador e `--check`
- `scripts/sync-desktop-apps.sh` — cria as apps por desktop remoto em falta
  (ex.: `Devbox.app`)

Atalho do picker (já no snapshot `herdr-config.toml`, ativado pelo instalador via
`scripts/sync-configs.sh`):

- `Cmd+Shift+K` → abre o Desktop Switcher

O instalador também garante as apps por desktop: cria as apps remotas em falta
(como `Devbox.app`, bundle id `com.gustavocaiano.herdr.devbox`) via
`scripts/sync-desktop-apps.sh`, sem recriar as que já existem. Para criar ou
verificar essas apps isoladamente:

```bash
~/.config/herd/scripts/sync-desktop-apps.sh
```

O instalador não escreve credenciais nem configuração SSH. O target remoto
`dev` (usado pelo `devbox`) tem de estar configurado em `~/.ssh/config` antes de
QA remoto.
