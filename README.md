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
cargo build --release                  # gera target/release/fly (~740 KB)
./target/release/fly exemplo.fly       # roda um arquivo
./target/release/fly -e 'console.log(2 ** 10)'
./target/release/fly --help
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

Medidos nesta máquina (Xeon 2,1 GHz, 4 vCPU), tempo de parede, mediana de 7 execuções, com o mesmo algoritmo em cada linguagem. Rode `benchmarks/run.sh` (ou `benchmarks/bench.py --fly <caminho>`). O script também confere se as saídas batem entre os runtimes.

| carga | fly | fly com PGO | Node 22 | Python 3 |
| --- | ---: | ---: | ---: | ---: |
| `fib(30)`, recursão | 135 ms | **102 ms** | 37 ms | 114 ms |
| laço de 20M com `%` | 366 ms | **292 ms** | 57 ms | 1465 ms |
| `map`/`filter`/`reduce` em 1M | 286 ms | **275 ms** | 93 ms | 134 ms |
| 1M objetos literais | 142 ms | **104 ms** | 30 ms | 214 ms |
| 200k concatenações de string | 8 ms | **8 ms** | 37 ms | 22 ms |

Os números do Node incluem cerca de 25 ms de inicialização do processo. A variação entre rodadas nesta máquina é de cerca de 10%. Antes desta rodada de otimização, o fly levava 270, 1482, 461, 276 e 14 ms nas mesmas cargas: sem PGO, ficou entre 1,6× e 4× mais rápido.

Outras medidas:

| | fly | Node 22 | Python 3 |
| --- | ---: | ---: | ---: |
| tamanho do executável | **736 KB** | 118 MB | — |
| inicialização (`-e 'console.log(1)'`) | **2 ms** | 24 ms | 15 ms |
| memória de pico, 1M inteiros em array | **18 MB** | 80 MB | 48 MB |

**Leitura honesta**: com PGO, o fly é mais rápido que o CPython em `fib` (por pouco), laço, objetos e strings. Ainda fica atrás em `map`/`filter`/`reduce`, porque cada callback paga o custo de uma chamada de função, que é o maior gargalo restante. O Node continua muito mais rápido em código pesado, porque compila para código de máquina com JIT. O fly não chega perto do Node em velocidade pura. O que ele entrega com vantagem clara é o tamanho, a inicialização e o consumo de memória.

**Build com PGO**: `scripts/pgo-build.sh` treina o binário com os próprios benchmarks e fixtures e recompila com esse perfil, em cerca de 25 s. Precisa do componente `llvm-tools` do rustup (`rustup component add llvm-tools`). Dá cerca de 15 a 25% nos benchmarks. O `cargo build --release` comum continua funcionando sem ele.

**Otimizações feitas**:

- **Superinstruções**: `local < constante` com salto, `local + - * % constante`, `x += e` e `x = x + e` sem empilhar o valor antigo, `x = e` e `x++` sem valor residual, `obj.prop` lido direto de uma variável, e laços com o teste no fim (um salto a menos por iteração).
- **Células só onde preciso**: uma variável vira célula (`Rc<RefCell>`) só quando uma função interna usa o nome dela. Antes, uma função com qualquer closure guardava todas as variáveis em células.
- **Chaves compartilhadas**: nomes de propriedade ficam numa tabela do bytecode e são `Rc<str>` reaproveitados. Objetos literais não copiam as chaves a cada criação.
- **Chamadas nativas sem alocação**: os argumentos vão para um buffer reaproveitado, e os callbacks de array recebem os valores por movimento.
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
  └─ builtins/     console, globais, array, string, json, fs/path
```

Pontos de projeto:

- **Módulos** são funções embrulhadas, como no CommonJS do Node: `exports`, `require`, `module`, `__filename`, `__dirname`.
- **Closures**: só as variáveis cujo nome é usado por uma função interna vão para células (`Rc<RefCell>`), que as funções internas compartilham. As demais ficam em slots da pilha, mesmo em funções que contêm closures.
- **Strings** são `Rc<String>`. Em `s += x` (e `s = s + x`) a string é estendida no lugar quando a referência é única, então montar uma string em laço é linear.
- **Event loop**: `nextTick` roda antes de qualquer timer. Timers ficam numa heap de (prazo, id). Erros não tratados imprimem `Uncaught …` e saem com código 1.
- **Pilha de recursão**: recursão JavaScript pura estoura em `RangeError` (10 mil frames). Chamadas que passam por built-ins (como um callback de `map`) são limitadas a 2 mil, e o interpretador roda numa thread com 256 MB de pilha.

## Testes

```sh
cargo test --release   # os fixtures rodam no binário otimizado
cargo test             # e no de debug, com as asserções de limites ligadas
```

Os scripts em `tests/fixtures/*.fly` são executados e a saída é comparada com os arquivos `.out`. Os `.out` foram conferidos contra o **Node.js 22**: todos batem, exceto `fs_path` e `require_local`, que dependem de decisões próprias do fly (`Buffer` e resolução de `.fly`). O fixture `fast_paths` cobre os caminhos otimizados: superinstruções com tipos mistos, `+=` cujo lado direito altera a própria variável, laços com `continue`, células só onde necessário e reuso de slots.

## Roadmap

1. **Registradores em vez de pilha** para expressões. Hoje cada operação passa pela pilha em memória, e isso domina as chamadas e os laços.
2. **Valores com NaN-boxing**, para que números não tenham contagem de referências nem destrutor.
3. **Callbacks sem reentrada** no interpretador, para `map`, `filter` e `reduce` ficarem tão rápidos quanto um laço comum.
4. **Caches inline** para propriedades (hoje busca linear) e para métodos de array (hoje comparação de texto a cada chamada).
5. **Promises, async/await, classes e destructuring**, que são as partes de sintaxe mais usadas do Node moderno.
6. **JIT** para código quente (por exemplo com Cranelift), o caminho para competir com o Node em código numérico.
