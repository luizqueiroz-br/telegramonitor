SHELL := /bin/bash
.DEFAULT_GOAL := help
BIN := target/release/telegram_cti_monitor
CARGO ?= cargo

.PHONY: help setup build run check clean messages dash export logout distclean

help: ## Mostra os comandos disponíveis
	@echo "Telegram CTI Monitor"
	@echo
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  make %-10s %s\n", $$1, $$2}'
	@echo

setup: ## Cria o .env e verifica as dependências do sistema
	@bash scripts/setup.sh

build: ## Compila em modo release
	@$(CARGO) build --release
	@echo "Binário: ./$(BIN)"

run: build ## Compila e executa o monitor (o primeiro run pede o código)
	@./$(BIN)

check: ## Checagem rápida de compilação
	@$(CARGO) check

messages: build ## Mostra as últimas mensagens gravadas no SQLite
	@./$(BIN) dump

dash: build ## Abre o dashboard no terminal (lê o messages.db local)
	@./$(BIN) dash

export: build ## Gera messages-export.db (sem WAL) para visualizador SQLite
	@./$(BIN) export

clean: ## Remove os artefatos de build
	@$(CARGO) clean

logout: ## Apaga a sessão salva (o próximo run pede login de novo)
	@rm -f monitor.session monitor.session-wal monitor.session-shm
	@echo "Sessão apagada."

distclean: clean logout ## Remove artefatos, sessão e o .env
	@rm -f .env
