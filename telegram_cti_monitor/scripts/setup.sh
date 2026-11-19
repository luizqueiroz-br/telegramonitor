#!/usr/bin/env bash
# Verifica dependências e prepara o .env. Não instala nada (sudo é com você).
set -uo pipefail
cd "$(dirname "$0")/.."

problems=0

say_ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; }
say_err()  { printf '  \033[31m✗\033[0m %s\n' "$1"; problems=$((problems + 1)); }

echo "Verificando dependências..."

# Rust
if command -v cargo >/dev/null 2>&1; then
    say_ok "cargo $(cargo --version | awk '{print $2}')"
else
    say_err "Rust/cargo não encontrado. Instale em https://rustup.rs"
fi

# Linker: o rustc usa 'cc' por padrão. Testa gerando e executando um binário real,
# pois um 'cc' falso no PATH responde a --version mas não produz binário.
tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT
echo 'int main(void){return 0;}' > "$tmpdir/t.c"
if cc "$tmpdir/t.c" -o "$tmpdir/t" >/dev/null 2>&1 && "$tmpdir/t" >/dev/null 2>&1; then
    say_ok "compilador C/linker: $(command -v cc)"
else
    cc_path=$(command -v cc || true)
    say_err "compilador C/linker não funcional${cc_path:+ ($cc_path)}"
    echo "      Instale: sudo apt-get update && sudo apt-get install -y build-essential"
    case "$cc_path" in
        "$HOME"/*) echo "      Atenção: '$cc_path' está no PATH e parece ser um shim falso; renomeie-o." ;;
    esac
fi

# curl (usado para enviar à API CTI)
if command -v curl >/dev/null 2>&1; then
    say_ok "curl $(curl --version | head -1 | awk '{print $2}')"
else
    say_err "curl não encontrado. Instale: sudo apt-get install -y curl"
fi

echo
if [ -f .env ]; then
    say_ok ".env já existe"
else
    cp .env.example .env
    say_ok ".env criado a partir de .env.example"
fi

echo
if [ "$problems" -ne 0 ]; then
    echo "Resolva os itens acima e rode 'make setup' novamente."
    exit 1
fi

if grep -qE '^TELEGRAM_(API_ID|API_HASH)=$' .env 2>/dev/null; then
    echo "Falta configurar o app do Telegram. Rode: make run"
    echo "  (ele pede API_ID/API_HASH/telefone; crie o app em https://my.telegram.org)"
else
    echo "Tudo pronto. Rode: make run"
fi
