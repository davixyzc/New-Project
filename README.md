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

Medido nesta máquina (Xeon 2,1 GHz, 4 vCPU), em tempo de parede. Cada número é a mediana de execuções intercaladas entre os runtimes, então a variação da máquina atinge todos igual, e o mesmo algoritmo roda em cada linguagem com a mesma saída. `benchmarks/bench.py` roda os programas com Node e Python e confere as saídas; o Bun foi medido com o mesmo critério num script à parte. **A máquina varia bastante entre sessões**: compare as colunas dentro de uma mesma tabela, nunca tabelas de sessões diferentes.

A coluna **fly** usa o binário com PGO (`scripts/pgo-build.sh`), com código nativo ligado. A coluna **interpretador** é o mesmo binário com `FLY_JIT=0` (mediana de 7 execuções). O melhor de cada linha está em negrito.

| carga | fly (JIT) | interpretador | Bun 1.4 | Node 22 | Python 3 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `fib(30)`, recursão | **13,4 ms** | 225 ms | 26,8 ms | 70,2 ms | 213 ms |
| laço de 20M com `%` | **32,0 ms** | 896 ms | 77,9 ms | 105,6 ms | 2799 ms |
| 1M `push`, depois `map`/`filter`/`reduce` | **67,8 ms** | 419 ms | 91,2 ms | 177,8 ms | 276 ms |
| 1M objetos literais | **5,8 ms** | 75 ms | 34,9 ms | 53,7 ms | 389 ms |
| 200k concatenações de string | **9,1 ms** | 13 ms | 29,2 ms | 68,1 ms | 42 ms |

Outras medidas, com o mesmo binário (a inicialização é a mediana de 15 execuções):

| | fly | Bun 1.4 | Node 22 | Python 3 |
| --- | ---: | ---: | ---: | ---: |
| tamanho do executável | **963 KB** | 107 MB | 123 MB | 6,5 MB |
| inicialização (`-e 'console.log(1)'`) | **3,4 ms** | 8,6 ms | 42,7 ms | 20,6 ms |
| memória de pico, 1M inteiros em array | **19 MB** | 57 MB | 81 MB | 49 MB |

**Leitura**: o fly ficou na frente do Bun e do Node em todas as cargas. A menor margem é a do `array`, com cerca de 1,35× sobre o Bun na mediana acima e cerca de 1,25× numa segunda medição, de 25 rodadas intercaladas (72 ms contra 90 ms). `fib` e o laço ficam perto de 2× e 2,4× sobre o Bun. Diferenças desse tamanho ainda dependem da máquina, então a medida mais estável é a contagem de instruções (`callgrind`), que não depende dela: sem PGO, o `array` caiu de 477 para 251 milhões de instruções (−47%) nesta etapa.

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
