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
