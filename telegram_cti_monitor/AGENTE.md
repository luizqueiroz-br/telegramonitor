# Agente de links de grupos

O agente lê os links `t.me` que o monitor já gravou em `messages.db`,
classifica cada grupo com um modelo do [OpenRouter](https://openrouter.ai) e
entra nos grupos classificados como fraude. Ele roda **dentro do `make run`** e
usa a mesma sessão do Telegram do monitor.

> Entrar em grupos é uma ação externa na sua conta. Leia os limites e riscos
> abaixo antes de ligar o modo autônomo.

## Como funciona

A cada varredura (padrão: 5 min):

1. pega mensagens novas com link (`messages.has_links = 1`);
2. extrai links de grupo (`t.me/+hash`, `t.me/joinchat/hash`, `t.me/usuario`,
   `tg://join?invite=...` e `tg://resolve?domain=...`), ignorando links
   reservados (share, proxy, addstickers etc.);
3. grava em `group_candidates` (um registro por link, sem duplicar);
4. antes de classificar, faz um *preview* sem entrar:
   - convite: `messages.checkChatInvite` (título, descrição, participantes,
     flags de scam/fake);
   - público: resolve o `@usuario` e confirma que é grupo;
   - já membro, canal ou entrada por aprovação são marcados como `skipped`/`already`;
5. manda link + título + descrição + (opcional) texto da mensagem para o modelo;
6. se `fraud = true` e `score >= AGENT_MIN_SCORE`, marca `approved`;
7. se o modo autônomo estiver ligado e houver orçamento, entra no grupo
   (`ImportChatInvite` ou `JoinChannel`) e marca `joined`.

A classificação usa `temperature = 0`, `max_tokens = 200` e no máximo
`AGENT_CLASSIFY_PER_SCAN` links por varredura, para controlar custo.

## Configuração

Coloque no `.env` (ou no ambiente):

```env
OPENROUTER_API_KEY=sk-or-...
OPENROUTER_MODEL=google/gemini-2.5-flash
AGENT_AUTO_JOIN=true
AGENT_DRY_RUN=false
AGENT_SEND_CONTEXT=true
AGENT_MIN_SCORE=0.8
```

Sem `AGENT_AUTO_JOIN=true` o agente só classifica (e deixa os aprovados na
tabela para revisão). Esse é o modo recomendado para os primeiros dias.

| Variável | Padrão | Efeito |
|---|---|---|
| `OPENROUTER_API_KEY` | — | sem ela o agente não liga |
| `OPENROUTER_MODEL` | `google/gemini-2.5-flash` | modelo do OpenRouter |
| `AGENT_ENABLED` | `true` | `false` desliga |
| `AGENT_AUTO_JOIN` | `false` | `true` entra sozinho; `false` só classifica (revisão manual) |
| `AGENT_DRY_RUN` | `false` | `true` nunca entra, só registra |
| `AGENT_SEND_CONTEXT` | `true` | envia o texto da mensagem como contexto |
| `AGENT_MIN_SCORE` | `0.8` | score mínimo para entrar |
| `AGENT_MAX_JOINS_PER_HOUR` | `2` | tentativas por hora (teto duro: 10) |
| `AGENT_MAX_JOINS_PER_DAY` | `6` | tentativas por dia (teto duro: 50) |
| `AGENT_CLASSIFY_PER_SCAN` | `5` | links classificados por varredura (máx. 20) |
| `AGENT_SCAN_SECS` | `300` | intervalo entre varreduras (mín. 30) |
| `AGENT_JOIN_DELAY_SECS` | `60` | pausa entre entradas (mín. 10) |

Modelos baratos para testar: `google/gemini-2.5-flash`,
`openai/gpt-4.1-mini`, `deepseek/deepseek-chat`. Confirme o id atual em
<https://openrouter.ai/models>.

## Limites contra ban

- Os limites contam **tentativas**, não só entradas bem-sucedidas. Uma falha
  também consome orçamento.
- `attempted_at` fica no SQLite, então reiniciar o monitor **não** zera a
  janela de 1 h / 1 dia.
- `FLOOD_WAIT` do Telegram interrompe as entradas da varredura e respeita o
  tempo pedido (limitado a 1 h).
- Há uma pausa (`AGENT_JOIN_DELAY_SECS`) entre entradas.
- Os padrões (2/h, 6/dia) são conservadores, mas **não há garantia** de que a
  conta não seja limitada. Contas novas e muitos grupos por dia são o cenário
  de maior risco. Aumente só se souber o que está fazendo.

Para revisar sem risco, rode com:

```env
AGENT_DRY_RUN=true
```

ou

```env
AGENT_AUTO_JOIN=false
```

## Tabela `group_candidates`

Estados: `pending` → `approved`/`skipped` → `joined`/`already`/`failed`/`error`.

Acompanhe:

```bash
sqlite3 messages.db "SELECT status, COUNT(*) FROM group_candidates GROUP BY status;"
sqlite3 messages.db "SELECT link, score, reason, status FROM group_candidates ORDER BY id DESC LIMIT 20;"
```

Reprocessar erros (ex.: depois de corrigir a chave/modelo):

```bash
sqlite3 messages.db "UPDATE group_candidates SET status='pending', tries=0 WHERE status IN ('error','failed');"
```

Forçar nova tentativa num grupo específico:

```bash
sqlite3 messages.db "UPDATE group_candidates SET status='approved', attempted_at=NULL WHERE link='https://t.me/+...';"
```

## Privacidade e dados enviados

- Com `AGENT_SEND_CONTEXT=true`, até 2.000 caracteres do texto da mensagem
  capturada (que pode conter PII) vão para o OpenRouter. Nome/telefone do
  remetente **não** são enviados.
- Para enviar só link, título, descrição e métricas do grupo, use
  `AGENT_SEND_CONTEXT=false`.
- O veredito, o modelo e o motivo ficam no SQLite local.

## Monitorar os grupos novos

O filtro `MONITORED_CHATS` vale para mensagens recebidas. Se ele estiver
preenchido, os grupos em que o agente entrar **não** serão coletados até você
adicionar os ids. Para coletar de tudo que o agente entrar, deixe
`MONITORED_CHATS` vazio.

## Solução de problemas

| Sintoma | Causa provável |
|---|---|
| `[agente] desativado (OPENROUTER_API_KEY ausente)` | falta a chave |
| `401` / `invalid api key` | chave errada ou sem crédito |
| `model not found` | id do modelo errado; confira em openrouter.ai/models |
| agente ligado mas nenhum link novo | não há `t.me` em `messages`, ou o link já foi visto |
| `FLOOD_WAIT` | Telegram pediu pausa; o agente para e tenta na próxima |
| `limite de entradas atingido` | orçamento por hora/dia; espere a janela |
| grupo entrou mas nada é coletado | `MONITORED_CHATS` está preenchido sem esse id |

## Aviso

Automatizar entrada em grupos pode violar os Termos do Telegram e expor a
conta usada. Os limites e o preview reduzem o risco, mas não eliminam ban,
limitação ou remoção. Use uma conta dedicada, mantenha o volume baixo e revise
os primeiros dias em `AGENT_DRY_RUN=true`.
