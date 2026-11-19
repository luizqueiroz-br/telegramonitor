# TUI — dashboard no terminal

Dashboard do `telegram_cti_monitor` para acompanhar as mensagens já gravadas
no SQLite. Ele **não chama o Telegram nem a API CTI**: lê apenas o
`messages.db` local, então pode ficar aberto ao lado do monitor.

## Como abrir

A partir de `telegram_cti_monitor/`:

```bash
make dash
```

Ou direto no binário já compilado:

```bash
./target/release/telegram_cti_monitor dash
```

O primeiro `make dash` compila em modo release (pode demorar). O banco é o
mesmo do monitor: `DB_PATH` (ambiente ou `.env`), com padrão `messages.db` no
diretório atual. O monitor e a TUI podem rodar juntos — o banco usa WAL.

Requisitos de terminal: um terminal interativo (TTY) com pelo menos ~80x24.
Redimensionar não quebra nada; a tela se ajusta.

## Tela

O cabeçalho tem três linhas:

1. totais gerais (`Mensagens`, `Chats`, `Remetentes`) e o contador
   `exibindo X de Y` da lista atual (X = carregadas, Y = total que casa com os
   filtros/janela);
2. status (`● ao vivo` / `⏸ pausado`), janela atual e atalhos;
3. resumo dos filtros ativos — ou os atalhos de filtro quando não há nenhum.

Abaixo fica o **feed** (tecla `s` alterna para as **estatísticas**).

## Teclas

### Globais

| Tecla | Ação |
|---|---|
| `q` | sai |
| `Ctrl+C` | sai de qualquer tela |
| `p` | pausa/retoma as atualizações automáticas (feed e estatísticas) |
| `s` | alterna feed ↔ estatísticas |
| `t` | troca a janela: 1 h → 24 h → 7 d → tudo |
| `r` | recarrega do zero |
| `c` | limpa todos os filtros |
| `l` | liga/desliga "só mensagens com links" |
| `/` | busca textual |
| `g` | escolhe o chat |
| `m` | escolhe o tipo de mensagem |

### Lista (feed)

| Tecla | Ação |
|---|---|
| `↑` / `↓` | linha anterior/próxima |
| `PgUp` / `PgDn` | uma página |
| `Home` / `End` | mais nova / mais antiga carregada |
| `Enter` | abre o detalhe da linha selecionada |

O feed ordena da mensagem **mais nova para a mais antiga**. Se você estiver na
primeira linha, mensagens novas aparecem sozinhas; se estiver navegando, a
linha em foco é mantida (inclusive ao pausar/retomar).

### Busca textual (`/`)

| Tecla | Ação |
|---|---|
| caracteres | digitam o termo |
| `Backspace` | apaga |
| `Enter` | aplica a busca |
| `Esc` | cancela e mantém o filtro anterior |

A busca é em `message_text`, sem diferenciar maiúsculas de minúsculas (ASCII).
`%` e `_` são tratados como texto, não como curinga. Termo vazio = sem filtro
de busca.

### Seletor de chat (`g`)

Digite parte do título ou do id para filtrar a lista. `↑`/`↓` escolhem,
`Enter` aplica e `Esc` cancela. A lista vem dos chats presentes em `messages`,
incluindo os que já não estão no catálogo `chats`.

### Seletor de tipo (`m`)

`↑`/`↓` escolhem entre `todos`, `text`, `photo`, `document`, `sticker`, `link`,
`poll`, `location`, `contact` e `other`. `Enter` aplica e `Esc` cancela.

### Detalhe

| Tecla | Ação |
|---|---|
| `↑` / `↓` | rola uma linha |
| `PgUp` / `PgDn` | rola uma página |
| `Home` / `End` | início / fim |
| `Esc` ou `Enter` | volta para a lista |
| `q` | sai do dashboard |

O detalhe mostra os campos preenchidos de `MESSAGE_COLUMNS`, mais o id no banco
e os caminhos de avatar do remetente (`users`) e do chat (`chats`), quando
existem. Nomes de `forward_*`/`reply_*` aparecem em amarelo e URLs ficam
sublinhadas. Textos longos quebram linha e podem ser rolados.

A imagem do avatar **não** é desenhada no terminal (cada terminal usa um
protocolo diferente); o caminho do arquivo é exibido.

## Filtros

Os filtros são combinados com **E** entre si e com a janela de tempo:

```
janela (t) + chat (g) + tipo (m) + só links (l) + busca (/)
```

- Ficam ativos até `c` (ou até serem trocados).
- Aparecem resumidos na terceira linha do cabeçalho.
- A lista é reconsultada a cada 2 s; a linha selecionada é mantida pelo id
  quando ainda casa com os filtros.
- Sem nenhum filtro, a tela mostra o feed ao vivo da janela atual.
- Se nada casar, aparece "Nenhuma mensagem casa com os filtros" (`c` limpa).

A janela `t` também vale para o feed. Com a janela padrão de 24 h, mensagens
mais antigas só aparecem depois de trocar para `7 d` ou `tudo`.

## Estatísticas (`s`)

Atualizadas a cada ~5 s (ou imediatamente ao abrir, trocar a janela ou
recarregar). `p` congela. Os painéis são:

| Painel | Conteúdo |
|---|---|
| Visão geral | total, chats e remetentes distintos, links (quantidade e %), última hora e últimas 24 h |
| Top chats | 10 chats com mais mensagens na janela |
| Top remetentes | 10 remetentes, com quantidade de chats distintos |
| Por tipo | contagem e barra por `message_type` na janela |
| Domínios | top domínios extraídos das mensagens com link na janela |
| Por hora UTC | 24 barras das últimas 24 h, com hora de pico |

As estatísticas usam **apenas a janela** (`t`). Filtros de chat, tipo, links e
busca valem para a lista, não para os números.

## Atualização automática

| O quê | Intervalo |
|---|---|
| feed / lista filtrada | 2 s |
| estatísticas | ~5 s |

`p` pausa tudo sem perder a posição; ao retomar, a lista é atualizada e a
linha em foco é preservada.

## Solução de problemas

| Sintoma | Causa provável / solução |
|---|---|
| feed vazio | nenhuma mensagem na janela — pressione `t` até `tudo` |
| "Nenhuma mensagem casa com os filtros" | pressione `c` |
| erro ao abrir o banco | rode de `telegram_cti_monitor/` ou defina `DB_PATH` |
| layout apertado | use pelo menos ~80x24; a tela se ajusta ao redimensionar |
| saiu e o terminal ficou estranho | `q`/`Ctrl+C` restauram; a TUI também restaura em erro/panic |

## Limitações

- Somente leitura das mensagens: não envia nada, não altera o Telegram nem a
  API CTI.
- Busca e filtros fazem varredura (`LIKE`/`WHERE`) no SQLite. Com um banco na
  casa das centenas de milhares de mensagens, vale criar índices em
  `timestamp` e `chat_id`.
- Sem renderização de imagens (avatares aparecem como caminho).
