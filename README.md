# fly

<p align="center">
  <img src="assets/fly-icon.svg" width="120" alt="Ícone do fly: um inseto roxo, com olhos azuis, sobre fundo escuro">
</p>

**fly** é uma linguagem de programação e um runtime no estilo Node.js: sintaxe parecida com JavaScript, módulos com `require`, `console`, timers e um event loop. A proposta é ser **mais leve**, com binário pequeno, sem dependências externas e inicialização quase instantânea.

Tudo está implementado em Rust, sem crates externas.

```js
// exemplo.fly
const saudar = (nome) => `Olá, ${nome}!`;
for (const p of ["Ana", "Bruno"]) console.log(saudar(p));
setTimeout(() => console.log("depois do código síncrono"), 10);
console.log("primeiro");
```

```
$ fly exemplo.fly
Olá, Ana!
Olá, Bruno!
primeiro
depois do código síncrono
```

## Como compilar e rodar

```sh
cargo build --release                  # gera target/release/fly (~850 KB)
./target/release/fly exemplo.fly       # roda um arquivo
./target/release/fly -e 'console.log(2 ** 10)'
./target/release/fly --help
FLY_JIT=0 ./target/release/fly exemplo.fly   # roda no interpretador, sem código nativo
```

Argumentos depois do arquivo chegam em `process.argv` (como no Node: `argv[0]` é o runtime e `argv[1]` é o script).

## O que a linguagem suporta

- **Declarações**: `let`, `const`, `function`, arrow functions, parâmetros padrão (`a = 1`) e rest (`...xs`).
- **Controle de fluxo**: `if`/`else`, `while`, `do`/`while`, `for`, `for…of`, `for…in`, `switch` (com fallthrough), `break`, `continue`, `return`.
- **Exceções**: `throw`, `try`/`catch`/`finally`. O `finally` roda também em `return`, `break` e `continue`.
- **Closures**: cada iteração de laço tem seu próprio `let`, como no JavaScript.
- **Objetos e funções**: literais de objeto (com métodos e atalhos `{ x }`), `this` em métodos e arrows, `new` com funções construtoras.
- **Strings**: aspas simples e duplas, template strings (`` `olá ${nome}` ``).
- **Spread**: `[...a, ...b]` e `f(...args)`.
- **Operadores**: aritméticos (`+ - * / % **`), comparação (`== != === !== < <= > >=`), `in`, lógicos (`&& || ??`), atribuição lógica (`&&= ||= ??=`, com alvo que é um nome ou uma propriedade de um nome), bit a bit (`& | ^ ~ << >> >>>`), ternário, `typeof`, `void`, vírgula, `++`/`--`, atribuição composta (`+=`, `&=` etc.).

## Biblioteca padrão

| Módulo / global | O que tem |
| --- | --- |
| `console` | `log`, `info`, `debug`, `error`, `warn` |
| `process` | `argv`, `env`, `platform`, `version`, `exit(code)`, `cwd()`, `nextTick(fn, ...args)` |
| timers | `setTimeout`, `setInterval`, `setImmediate`, `clearTimeout`, `clearInterval`, `clearImmediate` |
| `require` | módulos locais (`./arquivo.fly`, `./pasta` com `index.fly`), `fs` e `path` |
| `fs` | `readFileSync`, `writeFileSync`, `appendFileSync`, `existsSync`, `readdirSync`, `mkdirSync`, `unlinkSync`, `readFile(path, cb)`, `writeFile(path, data, cb)` |
| `path` | `join`, `resolve`, `basename`, `dirname`, `extname`, `sep` |
| `JSON` | `stringify(valor, _, indentação)` e `parse(texto)` |
| `Math` | `floor`, `ceil`, `round`, `trunc`, `abs`, `sqrt`, `cbrt`, `pow`, `min`, `max`, `hypot`, `imul`, `clz32`, `fround`, `sign`, `log`, `log2`, `log10`, `log1p`, `exp`, `expm1`, `sin`, `cos`, `tan`, `random`, `PI`, `E`, `LN2`, `LN10`, `LOG2E`, `LOG10E`, `SQRT2`, `SQRT1_2` |
| conversões | `Number` (com `isInteger`, `isSafeInteger`, `isFinite`, `isNaN`, `parseFloat`, `parseInt` e as constantes `MAX_SAFE_INTEGER`, `MIN_SAFE_INTEGER`, `EPSILON`, `MAX_VALUE`, `MIN_VALUE`), `String` (com `fromCharCode`), `Boolean`, `parseInt`, `parseFloat`, `isNaN`, `isFinite`, `NaN`, `Infinity` |
| erros | `Error`, `TypeError`, `RangeError`, `SyntaxError`, `ReferenceError` |
| `Object`, `Array`, `Date` | `Object.keys/values/entries/assign`, `Array.isArray/from`, `Date.now()` |

Métodos de **array**: `push pop shift unshift slice splice indexOf lastIndexOf includes join concat reverse map filter forEach reduce reduceRight find findIndex some every sort flat at fill toString` (`sort` é estável e aceita comparador).

Métodos de **string**: `length`, `charAt charCodeAt indexOf includes startsWith endsWith slice substring toUpperCase toLowerCase trim trimStart trimEnd split repeat padStart padEnd concat at codePointAt`, `fromCharCode` estático, e `replace` (só com padrão de texto, trocando a primeira ocorrência).

Métodos de **number**: `toFixed`, `toString` (com radix de 2 a 36).

## O que ainda não existe (e diferenças do JavaScript)

- Sem **classes**, **Map** e **Set**, **Promises**, **async/await**, **expressões regulares**, **destructuring**, **getters/setters** em literais, **generators**, **labels** (`rótulo:`), **`?.`**, **`delete`**, **`instanceof`**, **Symbol**, **BigInt**, arrays tipados, objeto `arguments`, `call`/`apply`/`bind`, `Array(n)` e `new Array(...)`, `globalThis`, e o construtor de `Date` (só `Date.now()`).
- `var` tem escopo de bloco, como `let`.
- Strings são indexadas por **caractere Unicode**, não por unidade UTF-16 (então `"😀".length` é 1, e no Node é 2).
- Os métodos de `fs` com callback fazem o I/O de forma síncrona e entregam o resultado pelo event loop. `readFileSync` retorna sempre uma string (o Node retorna `Buffer` sem encoding).
- `require` só resolve arquivos `.fly`. Não há `node_modules`.
- `toFixed` arredonda meio para cima no valor escalado, mas não reproduz 100% dos casos de ponto flutuante do V8.

## Desempenho (medido)

Medido nesta máquina (Xeon 2,1 GHz, 4 vCPU), em tempo de parede do processo inteiro, inicialização incluída. `benchmarks/bench.py` roda os 24 programas de `benchmarks/` com o fly, o Node 22 e o Bun 1.4, intercalados, e confere que as três saídas são iguais. Cada número é a mediana de 5 execuções. Cada programa existe em duas versões com o mesmo código, `.fly` e `.js`. Os números são do `cargo build --release` comum, sem PGO.

**A máquina varia bastante, até dentro de uma mesma sessão**: o `tak` foi de 98 a 147 ms em medições diferentes. Compare as colunas de uma mesma tabela, nunca tabelas de medições diferentes. Acima de 1× o fly ganha; abaixo, perde.

| benchmark | fly ms | Node ms | Bun ms | fly em relação ao melhor dos outros | resultado |
| --- | ---: | ---: | ---: | ---: | --- |
| `array` | 65,2 | 159,9 | 84,7 | 1,30× | fly ganha |
| `binary_trees` | 178,2 | 85,8 | 65,2 | 0,37× | perde |
| `char_codes` | 159,6 | 75,7 | 48,1 | 0,30× | perde |
| `closures` | 172,0 | 51,5 | 68,4 | 0,30× | perde |
| `console_lines` | 35,5 | 710,0 | 145,8 | 4,11× | fly ganha |
| `dict_keys` | 17,9 | 61,7 | 28,3 | 1,58× | fly ganha |
| `exceptions` | 57,6 | 414,7 | 69,2 | 1,20× | fly ganha |
| `fib` | 11,7 | 55,2 | 19,2 | 1,64× | fly ganha |
| `json` | 274,3 | 217,0 | 110,0 | 0,40× | perde |
| `life` | 2200,3 | 393,4 | 214,3 | 0,10× | perde |
| `loop` | 36,8 | 114,3 | 56,2 | 1,53× | fly ganha |
| `mandelbrot` | 21,8 | 61,3 | 24,1 | 1,11× | fly ganha |
| `matrix` | 123,6 | 55,4 | 28,2 | 0,23× | perde |
| `method_calls` | 197,7 | 60,6 | 19,7 | 0,10× | perde |
| `nbody_objects` | 492,1 | 70,6 | 31,1 | 0,06× | perde |
| `num_to_string` | 246,9 | 99,0 | 104,1 | 0,40× | perde |
| `objects` | 5,7 | 52,8 | 33,9 | 5,95× | fly ganha |
| `sieve` | 628,7 | 268,8 | 160,3 | 0,25× | perde |
| `sort_cmp` | 208,1 | 209,8 | 106,6 | 0,51× | perde |
| `split_join` | 107,0 | 115,7 | 78,3 | 0,73× | perde |
| `str_index` | 13,9 | 56,7 | 15,1 | 1,09× | fly ganha |
| `string` | 10,4 | 60,3 | 27,6 | 2,65× | fly ganha |
| `string_builder` | 121,8 | 155,5 | 93,3 | 0,77× | perde |
| `tak` | 111,5 | 79,3 | 27,9 | 0,25× | perde |

**Leitura**: o fly é mais rápido que o Node e o Bun em **10 de 24** programas. Ganha na inicialização (3,4 ms, contra 8,5 ms do Bun e 45,7 ms do Node), na saída de muitas linhas, nas exceções, em objetos que não escapam (substituição escalar), na recursão e nos laços numéricos, em `mandelbrot`, num objeto usado como dicionário (`dict_keys`, depois do índice por nome) e em strings.

Perde nos programas em que o tempo está em acessos a propriedades e elementos de arrays e em chamadas de função: `life` e `nbody_objects` ficam em 0,10× e 0,06× do melhor dos outros, `method_calls` em 0,10×, e `matrix` e `sieve` perto de 0,25×. Pelo `callgrind`, o custo está nos auxiliares que cada acesso chama, que copiam e soltam valores com contagem de referências, e na busca de propriedade sem cache.

**O objetivo de ser mais rápido em qualquer situação ainda não foi atingido**: 14 das 24 cargas ficam atrás de pelo menos um dos dois. O próximo passo para essas cargas é um JIT que compile acessos a arrays e propriedades para código nativo, com caches inline de propriedade e sem uma contagem de referências por leitura.

| | fly | Bun 1.4 | Node 22 |
| --- | ---: | ---: | ---: |
| tamanho do executável | **1,1 MB** | 107 MB | 123 MB |
| inicialização (`-e 'console.log(1)'`) | **3,4 ms** | 8,5 ms | 45,7 ms |

**Build com PGO**: `scripts/pgo-build.sh` treina o binário com os próprios benchmarks e fixtures e recompila com esse perfil. Precisa do componente `llvm-tools` do rustup (`rustup component add llvm-tools`). Nesta etapa o ganho foi pequeno e desigual: cerca de 6% a 13% no `array`, e nenhum ganho claro em `fib` e no laço. O `cargo build --release` comum já fica perto.

**Otimizações feitas**:

- **Código nativo (x86-64)**: cada função é traduzida para máquina na primeira vez que roda. O código trabalha sobre a mesma pilha de valores do interpretador, então os dois caminhos convivem. Números, variáveis locais, saltos e comparações ficam direto no código gerado; o resto chama a própria VM, e cada instrução tem uma só definição. Em outras plataformas, ou com `FLY_JIT=0`, roda o interpretador.
- **Locais de laço em registradores**: até seis locais usados dentro de laços ficam em `xmm2` a `xmm7`. Cada escrita atualiza o registrador e o quadro, então um contador não precisa passar pela memória a cada passo. Chamadas de auxiliares recarregam esses registradores, porque podem mudar qualquer local.
- **Resto com passo**: num laço cujo contador `i` só sobe de um em um e é dividido por uma constante (`i % 7`), o resto fica num registrador que sobe junto com `i`: soma um, compara com a constante e zera ao chegar nela. Não há multiplicação a cada passo. Um laço de 20M com `sum += i % 7` caiu de cerca de 55 para 41 ms, sem PGO. Se `i` começa negativo, ou muda de outro jeito dentro do laço, vale o cálculo normal.
- **Tipos conhecidos**: uma análise do bytecode prova quais locais e temporários guardam número em cada instrução (`jit/types.rs`), e o código nativo não verifica o tipo deles. Num laço de `%`, isso tirou cerca de um quinto das instruções.
- **Callbacks numéricos em lote**: `map`, `filter` e `reduce` com uma função que só faz conta com números rodam num laço nativo sobre o array, sem voltar ao Rust a cada elemento (`jit/region.rs`, `emit_batch`). O laço para no primeiro valor que não é número, e o interpretador segue dali, então o resultado é o mesmo.
- **Funções numéricas que se chamam**: uma função como `fib`, só de números, roda direto em código nativo, com pilha limitada a 1 MB. Se a pilha acabar, a chamada volta para o interpretador.
- **Superinstruções**: `local < constante` com salto, `local + - * % constante`, `x += e` e `x = x + e` sem empilhar o valor antigo, `x = e` e `x++` sem valor residual, `obj.prop` lido direto de uma variável, e laços com o teste no fim (um salto a menos por iteração).
- **Substituição escalar**: `const p = { x, y }` que não escapa vira duas variáveis, `p.x` e `p.y`, e o objeto não é criado. É o caso do benchmark de objetos, que caiu de 58 para 11 ms.
- **Células só onde preciso**: uma variável vira célula (`Rc<RefCell>`) só quando uma função interna usa o nome dela. Antes, uma função com qualquer closure guardava todas as variáveis em células.
- **Chaves compartilhadas**: nomes de propriedade ficam numa tabela do bytecode e são `Rc<str>` reaproveitados. Objetos literais não copiam as chaves a cada criação, e a busca compara primeiro o ponteiro.
- **Chamadas sem alocação**: os argumentos vão para um buffer reaproveitado, e os callbacks de array recebem os valores por movimento. Chamadas diretas de funções compiladas montam o quadro sem passar pelo caminho geral.
- **Chamadas diretas entre funções compiladas**: o código nativo chama o código do callee sem passar pelo despacho geral. Os quadros ficam num buffer de layout fixo (`FrameStack`), e a volta informa onde o quadro do chamador ficou, porque a pilha pode ter sido realocada.
- **`push` no código nativo**: `arr.push(v)`, com `arr` numa variável e `push` embutido, é feito por uma função direta (`rt::push_call`), sem montar a chamada geral. O valor vai para o array, e o novo tamanho substitui o `push`.
- **Inicialização**: o binário não tem `main` do Rust. O `main` do C (`#![no_main]`) pula a inicialização padrão do runtime, que custa cerca de 0,2 ms, e mantém o que o runtime fazia: `SIGPIPE` ignorado e a saída descarregada no fim. `--version` e `--help` não criam a thread do interpretador. A thread de 256 MB custa cerca de 0,5 ms, mas é ela que sustenta a liberação recursiva de estruturas longas (uma lista de 300 mil nós estoura uma pilha de 8 MB). A linkagem estática pouparia cerca de 0,7 ms, mas o binário passaria de 900 KB para 1,9 MB, então não é o padrão.
- **Escalares sem `drop`**: sobrescrever ou descartar um número não chama o destrutor do valor.

## Arquitetura

```
código-fonte
  └─ lexer.rs      tokens (com quebra de linha, para o ASI)
  └─ parser.rs     AST (ast.rs), descida recursiva + precedência
  └─ compiler.rs   bytecode por função: resolve escopos, slots, upvalues,
                   células para variáveis capturadas, finally embutido
  └─ vm.rs         máquina de pilha, frames, exceções por tabela de handlers,
                   propriedades, event loop com timers e nextTick
  └─ jit/          código nativo x86-64 por função (codegen.rs), análise de
                   números por slot (types.rs), laços e funções de números em
                   registradores (region.rs), funções que o código nativo
                   chama (rt.rs), montador (x64.rs) e memória executável (mem.rs)
  └─ builtins/     console, globais, array, string, json, fs/path
```

Pontos de projeto:

- **Módulos** são funções embrulhadas, como no CommonJS do Node: `exports`, `require`, `module`, `__filename`, `__dirname`.
- **Closures**: só as variáveis cujo nome é usado por uma função interna vão para células (`Rc<RefCell>`), que as funções internas compartilham. As demais ficam em slots da pilha, mesmo em funções que contêm closures.
- **Strings** são `Rc<String>`. Em `s += x` (e `s = s + x`) a string é estendida no lugar quando a referência é única, então montar uma string em laço é linear.
- **Código nativo**: a profundidade da pilha de operandos é conhecida em cada instrução, então cada operando tem um endereço fixo no quadro. O código nativo escreve diretamente só os valores sem referência. Para valores com referência, chama funções auxiliares que copiam, movem ou soltam o valor, e nos demais casos chama a própria VM, que usa o mesmo código do interpretador. O tamanho da pilha só é atualizado pelos helpers, que o definem antes de rodar.
- **Event loop**: `nextTick` roda antes de qualquer timer. Timers ficam numa heap de (prazo, id). Erros não tratados imprimem `Uncaught …` e saem com código 1.
- **Pilha de recursão**: recursão JavaScript pura estoura em `RangeError` (10 mil frames). Chamadas que passam por built-ins (como um callback de `map`) são limitadas a 2 mil, e o interpretador roda numa thread com 256 MB de pilha.

## Testes

```sh
cargo test --release   # os fixtures rodam no binário otimizado
cargo test             # e no de debug, com as asserções de limites ligadas
```

Os scripts em `tests/fixtures/*.fly` são executados duas vezes, com código nativo (o padrão) e no interpretador (`FLY_JIT=0`), e a saída é comparada com os arquivos `.out`. Os `.out` foram conferidos contra o **Node.js 22**: todos batem, exceto `fs_path` e `require_local`, que dependem de decisões próprias do fly (`Buffer` e resolução de `.fly`). Os fixtures `fast_paths`, `jit_numbers`, `jit_exceptions` e `native_paths` cobrem os caminhos otimizados: superinstruções com tipos mistos, zero negativo e `%` com sinal, NaN nas comparações, exceções que atravessam quadros nativos, recursão até o limite, closures, `switch`, e `try`/`finally` com `break` e `continue`. `jit_modulo` compara `%` por constante com `%` por divisor variável, inclusive o sinal do zero. `jit_loop_locals` muda o tipo de locais dentro de laços, chama funções e lança exceções. `scalar_objects` cobre a substituição de objetos. `native_calls` cobre chamadas diretas com a pilha crescendo, exceções que atravessam vários quadros, `push` com outros argumentos e callbacks com `this`.

## Roadmap

1. **Chamadas ainda mais baratas**: o código nativo empurra e tira os quadros, busca upvalues sem uma função auxiliar e acessa os closures com layout fixo, em vez de contar referências a cada chamada. É o que falta para `fib` e os callbacks chegarem a 2×.
2. **Registradores no código nativo**: os locais de laços já ficam em XMM. Falta estender a funções inteiras, aos temporários e às constantes.
3. **Valores com NaN-boxing**, para que valores não tenham tamanho de 16 bytes nem contagem de referências nos números.
4. **Caches inline** para propriedades (hoje busca linear) e para métodos de array (hoje a busca é pelo nome a cada chamada).
5. **Promises, async/await, classes e destructuring**, que são as partes de sintaxe mais usadas do Node moderno.
