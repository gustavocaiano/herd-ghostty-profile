# Troubleshooting

## O ícone mudou também na Ghostty normal

`macos-icon` é app-wide para o bundle Ghostty. Não é por janela/profile. Remove `macos-icon` do profile se não quiseres esse comportamento, ou usa `Herdr.app` separada.

## `Ctrl+Cmd+↑` fica preso no UI `ctrl+b`

Não uses `ctrl+b + arrow` para navegação persistente; Herdr mantém o navigate mode ativo. A config deste repo usa bindings diretos:

```toml
previous_workspace = "ctrl+up"
next_workspace = "ctrl+down"
```

e Ghostty traduz `Ctrl+Cmd+↑/↓` para `Ctrl+↑/↓`.

## `Cmd+N` deixou de criar workspace depois do Herdr 0.6

No Herdr 0.6, `prefix+n` passou a ser navegação para o próximo tab. O novo workspace é `prefix+shift+n`.

No profile Ghostty Herdr, usa:

```ini
keybind = super+n=text:\x02N
```

Isto envia `Ctrl+B` seguido de `Shift+N`.

## `Herdr.app` não abre Herdr

Recria a app:

```bash
~/.config/herd/scripts/create-herdr-app.sh
```

Depois abre diretamente:

```bash
open "$HOME/Applications/Herdr.app"
```

Se vires `RBSRequestErrorDomain Code=5` / `Launchd job spawn failed`, recria com a versão atual dos scripts. Versões antigas usavam um shell script como `CFBundleExecutable`; a versão atual usa um launcher nativo compilado, que é mais compatível com LaunchServices.

## `nested herdr is disabled by default`

Isto acontece quando lanças `Herdr.app` a partir de dentro de um pane Herdr e o ambiente herda `HERDR_ENV=1`. Recria a app com a versão atual dos scripts:

```bash
~/.config/herd/scripts/create-herdr-app.sh
```

O launcher nativo atual limpa `HERDR_ENV`, `HERDR_SOCKET_PATH` e variáveis `HERDR_ACTIVE_*` antes de iniciar Ghostty/Herdr.

## macOS diz que a app não é verificada

A cópia é assinada localmente/ad-hoc. Para uso pessoal, normalmente basta abrir explicitamente uma vez pelo Finder ou permitir em Privacy & Security se o macOS bloquear.

## Depois de atualizar Ghostty, Herdr.app ficou antiga

Corre:

```bash
~/.config/herd/scripts/update-herdr-app.sh
```

Ou instala o updater diário em `docs/updater.md`.

## A app de um desktop remoto (ex.: `Devbox.app`) não existe

Corre:

```bash
~/.config/herd/scripts/sync-desktop-apps.sh
```

Isto cria apenas as apps em falta — uma por desktop remoto, com o nome efetivo
do desktop (`app_name` ou `label`) e bundle id `com.gustavocaiano.herdr.<id>` —
e nunca recria apps que já existam. O instalador do Desktop Switcher também
corre este passo.

## A app de um desktop remoto ficou desatualizada depois de atualizar a Ghostty

O updater (`scripts/update-herdr-app.sh`) processa `Herdr.app` e todas as apps
por desktop de forma independente e recria as desatualizadas. Se a app estiver
aberta, é skipada por segurança — fecha-a e volta a correr o updater.

## O `--check` reporta assinatura inválida ou bundle id errado numa app por desktop

O `install-desktop-switcher.sh --check` verifica presença, assinatura e bundle
id de cada app por desktop. Para corrigir, recria a app manualmente (exemplo
para o `devbox`):

```bash
APP_NAME="Devbox" BUNDLE_ID="com.gustavocaiano.herdr.devbox" \
  TARGET_APP="$HOME/Applications/Devbox.app" \
  ~/.config/herd/scripts/create-herdr-app.sh
```

## O primeiro lançamento do `devbox` depois das apps por desktop falha com "configuration changed while pid N is still open"

Uma janela antiga do cliente devbox (do design de bundle único `Herdr.app`)
continua aberta e o registo dela em `~/.local/state/herdr-desktop-switcher/`
ainda aponta para o bundle/caminho partilhados. Fecha essa janela antiga uma
vez; o registo obsoleto é removido e o lançamento seguinte usa a `Devbox.app`
normalmente.
