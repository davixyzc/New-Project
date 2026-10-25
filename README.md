# fly

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
- **Operadores**: aritméticos (`+ - * / % **`), comparação (`== != === !== < <= > >=`), lógicos (`&& || ??`), ternário, `typeof`, `++`/`--`, atribuição composta (`+=` etc.).

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
| `Math` | `floor`, `ceil`, `round`, `trunc`, `abs`, `sqrt`, `pow`, `min`, `max`, `sign`, `log`, `exp`, `sin`, `cos`, `tan`, `random`, `PI`, `E` |
| conversões | `Number`, `String`, `Boolean`, `parseInt`, `parseFloat`, `isNaN`, `isFinite`, `NaN`, `Infinity` |
| erros | `Error`, `TypeError`, `RangeError`, `SyntaxError`, `ReferenceError` |
| `Object`, `Array`, `Date` | `Object.keys/values/entries`, `Array.isArray/from`, `Date.now()` |

Métodos de **array**: `push pop shift unshift slice indexOf includes join concat reverse map filter forEach reduce find findIndex some every sort` (`sort` é estável e aceita comparador).

Métodos de **string**: `length`, `charAt charCodeAt indexOf includes startsWith endsWith slice substring toUpperCase toLowerCase trim trimStart trimEnd split repeat padStart padEnd concat`, e `replace` (só com padrão de texto, trocando a primeira ocorrência).

Métodos de **number**: `toFixed`, `toString`.

## O que ainda não existe (e diferenças do JavaScript)

- Sem **classes**, **Promises**, **async/await**, **expressões regulares**, **destructuring**, **getters/setters**, **generators**, **labels**, **operadores bit a bit**, objeto `arguments` e `new Array(...)`.
- `var` tem escopo de bloco, como `let`.
- Strings são indexadas por **caractere Unicode**, não por unidade UTF-16 (então `"😀".length` é 1, e no Node é 2).
- Os métodos de `fs` com callback fazem o I/O de forma síncrona e entregam o resultado pelo event loop. `readFileSync` retorna sempre uma string (o Node retorna `Buffer` sem encoding).
- `require` só resolve arquivos `.fly`. Não há `node_modules`.
- `toFixed` arredonda meio para cima no valor escalado, mas não reproduz 100% dos casos de ponto flutuante do V8.

## Desempenho (medido)

Medidos nesta máquina (Xeon 2,1 GHz, 4 vCPU), tempo de parede, com a mediana de 7 execuções (5 no interpretador) e o mesmo algoritmo em cada linguagem. `benchmarks/bench.py --fly <caminho>` roda tudo e confere se as saídas batem entre os runtimes. A máquina varia entre sessões (nesta, o Node ficou mais lento que antes), então compare as colunas dentro de uma mesma tabela.

A coluna **fly (JIT)** usa o binário com PGO (`scripts/pgo-build.sh`), com código nativo ligado. A coluna **interpretador** é o mesmo binário com `FLY_JIT=0`. O melhor de cada linha está em negrito.

| carga | fly (JIT) | fly (interpretador) | Node 22 | Python 3 |
| --- | ---: | ---: | ---: | ---: |
| `fib(30)`, recursão | 73 ms | 169 ms | **38 ms** | 118 ms |
| laço de 20M com `%` | **49 ms** | 456 ms | 63 ms | 1561 ms |
| `map`/`filter`/`reduce` em 1M | 241 ms | 354 ms | **87 ms** | 146 ms |
| 1M objetos literais | **11 ms** | 66 ms | 30 ms | 229 ms |
| 200k concatenações de string | **8 ms** | 8 ms | 38 ms | 30 ms |

Outras medidas, com o binário com PGO (a inicialização é a mediana de 15 execuções):

| | fly | Node 22 | Python 3 |
| --- | ---: | ---: | ---: |
| tamanho do executável | **909 KB** (PGO); 853 KB sem PGO | 123 MB | — |
| inicialização (`-e 'console.log(1)'`) | **1,8 ms** | 25 ms | 14 ms |
| memória de pico, 1M inteiros em array | **18 MB** | 77 MB | 47 MB |

**Leitura honesta**: o fly é o mais rápido em laço, objetos e strings, e tem o menor executável, a inicialização mais curta e o menor consumo de memória. Em `fib` ele fica em segundo: é mais rápido que o CPython (73 contra 118 ms), mas o Node faz em 38 ms, com um JIT que especializa tipos e embute chamadas. A vitória no laço é estreita, e em outra sessão o Node ficou em 56 ms. Em `map`/`filter`/`reduce` o fly fica em último (241 ms, contra 146 do Python e 87 do Node), e essa é a maior lacuna. A chamada de função é o gargalo: cada callback monta um quadro com contagem de referências, passa por várias camadas de `Result` e de Rust e mexe na pilha de valores. Medido com `callgrind`, são cerca de 380 instruções por chamada.

**Build com PGO**: `scripts/pgo-build.sh` treina o binário com os próprios benchmarks e fixtures e recompila com esse perfil, em cerca de 25 s. Precisa do componente `llvm-tools` do rustup (`rustup component add llvm-tools`). Dá cerca de 15 a 25% nos benchmarks. O `cargo build --release` comum continua funcionando sem ele.

**Otimizações feitas**:

- **Código nativo (x86-64)**: cada função é traduzida para máquina na primeira vez que roda. O código trabalha sobre a mesma pilha de valores do interpretador, então os dois caminhos convivem. Números, variáveis locais, saltos e comparações ficam direto no código gerado; o resto chama a própria VM, e cada instrução tem uma só definição. Em outras plataformas, ou com `FLY_JIT=0`, roda o interpretador.
- **Locais de laço em registradores**: até oito locais usados dentro de laços ficam em `xmm2` a `xmm9`. Cada escrita atualiza o registrador e o quadro, então um contador não precisa passar pela memória a cada passo. Um laço vazio de 20M passos caiu de cerca de 56 para 18 ms (sem PGO). Chamadas de auxiliares recarregam esses registradores, porque podem mudar qualquer local.
- **Tipos conhecidos**: uma análise do bytecode prova quais locais e temporários guardam número em cada instrução (`jit/types.rs`), e o código nativo não verifica o tipo deles. Num laço de `%`, isso tirou cerca de um quinto das instruções.
- **Superinstruções**: `local < constante` com salto, `local + - * % constante`, `x += e` e `x = x + e` sem empilhar o valor antigo, `x = e` e `x++` sem valor residual, `obj.prop` lido direto de uma variável, e laços com o teste no fim (um salto a menos por iteração).
- **Substituição escalar**: `const p = { x, y }` que não escapa vira duas variáveis, `p.x` e `p.y`, e o objeto não é criado. É o caso do benchmark de objetos, que caiu de 58 para 11 ms.
- **Células só onde preciso**: uma variável vira célula (`Rc<RefCell>`) só quando uma função interna usa o nome dela. Antes, uma função com qualquer closure guardava todas as variáveis em células.
- **Chaves compartilhadas**: nomes de propriedade ficam numa tabela do bytecode e são `Rc<str>` reaproveitados. Objetos literais não copiam as chaves a cada criação, e a busca compara primeiro o ponteiro.
- **Chamadas sem alocação**: os argumentos vão para um buffer reaproveitado, e os callbacks de array recebem os valores por movimento. Chamadas diretas de funções compiladas montam o quadro sem passar pelo caminho geral.
- **Escalares sem `drop`**: sobrescrever ou descartar um número não chama o destrutor do valor.
- **Testado e descartado**: divisão por constante com multiplicação mágica (Hacker's Delight, figura 10-1). Estava correta e foi testada contra `%`, mas nesta CPU o `idiv` de 32 bits continuou mais rápido, e o código saiu.

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
                   números por slot (types.rs), funções que o código nativo
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

Os scripts em `tests/fixtures/*.fly` são executados duas vezes, com código nativo (o padrão) e no interpretador (`FLY_JIT=0`), e a saída é comparada com os arquivos `.out`. Os `.out` foram conferidos contra o **Node.js 22**: todos batem, exceto `fs_path` e `require_local`, que dependem de decisões próprias do fly (`Buffer` e resolução de `.fly`). Os fixtures `fast_paths`, `jit_numbers`, `jit_exceptions` e `native_paths` cobrem os caminhos otimizados: superinstruções com tipos mistos, zero negativo e `%` com sinal, NaN nas comparações, exceções que atravessam quadros nativos, recursão até o limite, closures, `switch`, e `try`/`finally` com `break` e `continue`. `jit_modulo` compara `%` por constante com `%` por divisor variável, inclusive o sinal do zero. `jit_loop_locals` muda o tipo de locais dentro de laços, chama funções e lança exceções. `scalar_objects` cobre a substituição de objetos.

## Roadmap

1. **Chamadas mais baratas**: reaproveitar um quadro nos callbacks de array e guardar os quadros em memória bruta, em vez de `Vec<Frame>` com contagem de referências por chamada. É o que mais falta para `map`/`filter`/`reduce` chegarem perto de 100 ms, e para `fib` alcançar o Node.
2. **Registradores no código nativo**: os locais de laços já ficam em XMM. Falta estender a funções inteiras, aos temporários e às constantes.
3. **Valores com NaN-boxing**, para que valores não tenham tamanho de 16 bytes nem contagem de referências nos números.
4. **Caches inline** para propriedades (hoje busca linear) e para métodos de array (hoje a busca é pelo nome a cada chamada).
5. **Promises, async/await, classes e destructuring**, que são as partes de sintaxe mais usadas do Node moderno.
