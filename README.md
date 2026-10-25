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
cargo build --release                  # gera target/release/fly (~730 KB)
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

Medidos nesta máquina (4 vCPU), tempo de parede, uma execução por ferramenta, com o mesmo algoritmo em cada linguagem. Os scripts estão em `benchmarks/`; rode `benchmarks/run.sh`.

| carga | fly | Node 22 | Python 3 |
| --- | ---: | ---: | ---: |
| `fib(30)`, recursão | 285 ms | 46 ms | 105 ms |
| laço de 20M com `%` | 1479 ms | 62 ms | 1718 ms |
| `map`/`filter`/`reduce` em 1M | 481 ms | 86 ms | 134 ms |
| 1M objetos literais | 323 ms | 31 ms | 281 ms |
| 200k concatenações de string | 19 ms | 57 ms | 44 ms |

Outras medidas:

| | fly | Node 22 | Python 3 |
| --- | ---: | ---: | ---: |
| tamanho do executável | **727 KB** | 118 MB | — |
| inicialização (`-e 'console.log(1)'`) | **2 ms** | 27 ms | 15 ms |
| memória de pico, 1M inteiros em array | **18 MB** | 80 MB | 48 MB |

**Leitura honesta**: o fly ainda **não é "ultra rápido"**. Ele fica na faixa de um interpretador como o CPython: à frente só em laço numérico e concatenação de string, e atrás em chamadas de função, `map`/`filter` e criação de objetos. O Node é bem mais rápido em código pesado, porque usa JIT. O que o fly já entrega com vantagem clara é o **tamanho**, a **inicialização** e o **consumo de memória**, o que importa para scripts e ferramentas de linha de comando. Ganhar velocidade de verdade exige as etapas do roadmap abaixo.

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
- **Closures**: uma função que contém outras funções guarda suas variáveis em células (`Rc<RefCell>`), que as funções internas compartilham. Funções sem closures usam só slots da pilha.
- **Strings** são `Rc<String>`. Em `s += x` (e `s = s + x`) a string é estendida no lugar quando a referência é única, então montar uma string em laço é linear.
- **Event loop**: `nextTick` roda antes de qualquer timer. Timers ficam numa heap de (prazo, id). Erros não tratados imprimem `Uncaught …` e saem com código 1.
- **Pilha de recursão**: recursão JavaScript pura estoura em `RangeError` (10 mil frames). Chamadas que passam por built-ins (como um callback de `map`) são limitadas a 2 mil, e o interpretador roda numa thread com 256 MB de pilha.

## Testes

```sh
cargo test --release
```

Os scripts em `tests/fixtures/*.fly` são executados e a saída é comparada com os arquivos `.out`. Os `.out` foram conferidos contra o **Node.js 22**: todos batem, exceto `fs_path` e `require_local`, que dependem de decisões próprias do fly (`Buffer` e resolução de `.fly`).

## Roadmap

1. **VM com registradores** e instruções fundidas para operações com constantes, reduzindo o tráfego da pilha. É o próximo passo mais direto para laços e chamadas.
2. **Valores com NaN-boxing**, para evitar contagem de referências em cada cópia de número.
3. **Caches inline** para acesso a propriedades (hoje é busca linear nas chaves do objeto).
4. **Promises, async/await, classes e destructuring**, que são as partes de sintaxe mais usadas do Node moderno.
5. **JIT** para código quente (por exemplo com Cranelift), o caminho para competir com o Node em código numérico.
