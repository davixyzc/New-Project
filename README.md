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
cargo build --release                  # gera target/release/fly (~1,2 MB)
./target/release/fly exemplo.fly       # roda um arquivo
./target/release/fly -e 'console.log(2 ** 10)'
./target/release/fly --help
FLY_JIT=0 ./target/release/fly exemplo.fly   # roda no interpretador, sem código nativo
```

Argumentos depois do arquivo chegam em `process.argv` (como no Node: `argv[0]` é o runtime e `argv[1]` é o script).

## O que a linguagem suporta

- **Declarações**: `let`, `const`, `var` (com escopo de função, como no JavaScript), `function`, arrow functions, parâmetros padrão (`a = 1`) e rest (`...xs`).
- **Desestruturação**: de arrays e de objetos, em declarações (`const [a, b] = par`, `const { x, y: z = 0, ...resto } = obj`), em parâmetros, na cabeça de `for…of` e `for…in`, e como atribuição (`[a, b] = [b, a]`). Aceita aninhamento, padrões e restos.
- **Classes**: `class` com `constructor`, métodos, `get` e `set`, membros `static`, campos de instância e estáticos (`contador = 0;`), `extends`, `super(...)` e `super.metodo()`, e expressões de classe. Uma instância é impressa com o nome da classe, como no Node.
- **Controle de fluxo**: `if`/`else`, `while`, `do`/`while`, `for`, `for…of`, `for…in`, `switch` (com fallthrough), `break`, `continue`, `return`.
- **Exceções**: `throw`, `try`/`catch`/`finally`. O `finally` roda também em `return`, `break` e `continue`. Os erros são instâncias de `Error`, `TypeError`, `RangeError`, `SyntaxError` e `ReferenceError`, e uma classe pode estender `Error`.
- **Closures**: cada iteração de laço tem seu próprio `let` ou `const`, como no JavaScript, e um `var` é um só para a função inteira.
- **Objetos e funções**: literais de objeto (com métodos e atalhos `{ x }`), `this` em métodos e arrows, `new`, `instanceof` (com classes, funções construtoras, `Map`, `Set`, `Error` e as tabelas `Object` e `Array`), `delete`, `hasOwnProperty` e `toString`. O `toString` de uma classe é usado por `+`, por template strings, por `String()` e por `join`.
- **Coleções**: `Map` e `Set`, com ordem de inserção e igualdade de `SameValueZero` (`NaN` é igual a `NaN`, e `+0` é igual a `-0`). Funcionam com `for…of`, spread e `Array.from`.
- **Strings**: aspas simples e duplas, template strings (`` `olá ${nome}` ``), e os escapes `\uXXXX` e `\xXX`.
- **Spread**: `[...a, ...b]` e `f(...args)`.
- **Números**: literais decimais, hexadecimais, binários e octais, com `_` entre dígitos (`1_000`).
- **Operadores**: aritméticos (`+ - * / % **`), comparação (`== != === !== < <= > >=`), `in`, `instanceof`, lógicos (`&& || ??`), atribuição lógica (`&&= ||= ??=`, com alvo que é um nome ou uma propriedade de um nome), bit a bit (`& | ^ ~ << >> >>>`), ternário, `typeof`, `void`, `delete`, vírgula, `++`/`--` e atribuição composta (`+=`, `&=` etc.).

## Biblioteca padrão

| Módulo / global | O que tem |
| --- | --- |
| `console` | `log`, `info`, `debug`, `error`, `warn`. Objetos aparecem como no Node: `Classe { campo: valor }`, `Map(2) { 'a' => 1 }`, `Set(1) { 2 }`, e `-0` com o sinal |
| `process` | `argv`, `env`, `platform`, `version`, `exit(code)`, `cwd()`, `nextTick(fn, ...args)` |
| timers | `setTimeout`, `setInterval`, `setImmediate`, `clearTimeout`, `clearInterval`, `clearImmediate` |
| `require` | módulos locais (`./arquivo.fly`, `./pasta` com `index.fly`), `fs` e `path` |
| `fs` | `readFileSync`, `writeFileSync`, `appendFileSync`, `existsSync`, `readdirSync`, `mkdirSync`, `unlinkSync`, `readFile(path, cb)`, `writeFile(path, data, cb)` |
| `path` | `join`, `resolve`, `basename`, `dirname`, `extname`, `sep` |
| `JSON` | `stringify(valor, _, indentação)` e `parse(texto)` |
| `Math` | `floor`, `ceil`, `round`, `trunc`, `abs`, `sqrt`, `cbrt`, `pow`, `min`, `max`, `hypot`, `imul`, `clz32`, `fround`, `sign`, `log`, `log2`, `log10`, `log1p`, `exp`, `expm1`, `sin`, `cos`, `tan`, `random`, `PI`, `E`, `LN2`, `LN10`, `LOG2E`, `LOG10E`, `SQRT2`, `SQRT1_2` |
| conversões | `Number` (com `isInteger`, `isSafeInteger`, `isFinite`, `isNaN`, `parseFloat`, `parseInt` e as constantes `MAX_SAFE_INTEGER`, `MIN_SAFE_INTEGER`, `EPSILON`, `MAX_VALUE`, `MIN_VALUE`), `String` (com `fromCharCode`), `Boolean`, `parseInt`, `parseFloat`, `isNaN`, `isFinite`, `NaN`, `Infinity` |
| `Map`, `Set` | construtores com iterável opcional (ver abaixo) |
| erros | `Error`, `TypeError`, `RangeError`, `SyntaxError`, `ReferenceError` |
| `Object` | `keys`, `values`, `entries`, `assign`, `is`, `create`, `getPrototypeOf` |
| `Array`, `Date` | `Array.isArray`, `Array.from`, `Date.now()` |
| `globalThis` | um objeto com cópias dos globais embutidos |

Métodos de **array**: `push pop shift unshift slice splice indexOf lastIndexOf includes join concat reverse map filter forEach reduce reduceRight find findIndex some every sort flat at fill toString` (`sort` é estável e aceita comparador).

Métodos de **string**: `length`, `charAt charCodeAt indexOf includes startsWith endsWith slice substring toUpperCase toLowerCase trim trimStart trimEnd split repeat padStart padEnd concat at codePointAt`, `fromCharCode` estático, e `replace` (só com padrão de texto, trocando a primeira ocorrência).

Métodos de **number**: `toFixed`, `toString` (com radix de 2 a 36).

Métodos de **Map**: `get set has delete clear forEach keys values entries`, e a propriedade `size`. Métodos de **Set**: `add has delete clear forEach keys values entries`, e `size`.

## O que ainda não existe (e diferenças do JavaScript)

- Sem **Promises**, **async/await**, **generators**, **Symbol** (e o protocolo de iteração de objetos próprios, com `[Symbol.iterator]`), **expressões regulares**, **`?.`**, **labels** (`rótulo:`), **`arguments`**, **`call`/`apply`/`bind`**, **`new.target`**, **campos privados** (`#x`), **blocos `static`**, **getters e setters em literais de objeto**, **BigInt**, **template strings com tag**, **`Object.defineProperty`** e **`Object.freeze`**, **`Array(n)`** e **`new Array(...)`**, e o construtor de `Date` (só `Date.now()`).
- Sem **TDZ**: um `let` ou `const` lido antes da declaração devolve `undefined`, sem erro.
- Uma `class` pode ser chamada sem `new`, sem erro. `this` antes de `super()` não é verificado. Os inicializadores de campos estáticos rodam no escopo de fora da classe, e não com a classe como `this`.
- Não há `Object.prototype`. Por isso `Object.getPrototypeOf` de um objeto comum devolve `null`, onde o Node devolveria `Object.prototype`. Uma classe não pode estender `Map` nem `Set`: a instância não é um `Map`. Estender `Error` funciona.
- `Map` e `Set`: `for…of` e o spread percorrem uma cópia da coleção, e `keys()`, `values()` e `entries()` devolvem arrays, sem `next()`. Um `forEach` pula as entradas removidas antes da vez e vê os valores trocados, mas não visita as entradas adicionadas durante o laço.
- Strings são indexadas por **caractere Unicode**, não por unidade UTF-16 (então `"😀".length` é 1, e no Node é 2).
- Os métodos de `fs` com callback fazem o I/O de forma síncrona e entregam o resultado pelo event loop. `readFileSync` retorna sempre uma string (o Node retorna `Buffer` sem encoding).
- `require` só resolve arquivos `.fly`. Não há `node_modules`.
- `toFixed` arredonda meio para cima no valor escalado, mas não reproduz 100% dos casos de ponto flutuante do V8.

## Desempenho (medido)

Medido nesta máquina (Xeon 2,1 GHz, 4 vCPU), em tempo de parede do processo inteiro, inicialização incluída. `benchmarks/bench.py` roda os 24 programas de `benchmarks/` com o fly, o Node 22 e o Bun 1.4, intercalados, e confere que as três saídas são iguais. Cada número é a mediana de 5 execuções. Cada programa existe em duas versões com o mesmo código, `.fly` e `.js`. Os números são do `cargo build --release` comum, sem PGO.

**A máquina varia bastante, até dentro de uma mesma sessão**: o `tak` foi de 98 a 147 ms em medições diferentes, e os casos perto de 1× trocam de lado de uma rodada para outra. Compare as colunas de uma mesma tabela, nunca tabelas de medições diferentes. Acima de 1× o fly ganha; abaixo, perde.

| benchmark | fly ms | Node ms | Bun ms | fly em relação ao melhor dos outros | resultado |
| --- | ---: | ---: | ---: | ---: | --- |
| `array` | 76,8 | 186,9 | 92,7 | 1,21× | fly ganha |
| `binary_trees` | 203,1 | 78,6 | 66,0 | 0,32× | perde |
| `char_codes` | 149,2 | 82,0 | 52,0 | 0,35× | perde |
| `closures` | 260,8 | 59,1 | 92,1 | 0,23× | perde |
| `console_lines` | 27,9 | 751,1 | 160,0 | 5,73× | fly ganha |
| `dict_keys` | 24,4 | 92,3 | 44,8 | 1,84× | fly ganha |
| `exceptions` | 63,6 | 425,3 | 99,8 | 1,57× | fly ganha |
| `fib` | 14,7 | 79,4 | 26,0 | 1,77× | fly ganha |
| `json` | 333,6 | 267,0 | 122,0 | 0,37× | perde |
| `life` | 2370,0 | 398,4 | 247,9 | 0,10× | perde |
| `loop` | 34,3 | 106,4 | 54,4 | 1,59× | fly ganha |
| `mandelbrot` | 23,5 | 81,5 | 30,9 | 1,31× | fly ganha |
| `matrix` | 139,8 | 66,9 | 29,7 | 0,21× | perde |
| `method_calls` | 214,0 | 63,0 | 23,1 | 0,11× | perde |
| `nbody_objects` | 376,3 | 82,8 | 32,0 | 0,08× | perde |
| `num_to_string` | 152,7 | 107,7 | 105,9 | 0,69× | perde |
| `objects` | 5,6 | 56,0 | 32,6 | 5,80× | fly ganha |
| `sieve` | 503,5 | 267,3 | 165,0 | 0,33× | perde |
| `sort_cmp` | 220,4 | 207,0 | 111,9 | 0,51× | perde |
| `split_join` | 92,2 | 119,7 | 64,7 | 0,70× | perde |
| `str_index` | 11,5 | 42,7 | 11,0 | 0,96× | perde |
| `string` | 14,8 | 66,9 | 33,7 | 2,27× | fly ganha |
| `string_builder` | 114,4 | 166,9 | 113,8 | 1,00× | perde |
| `tak` | 126,0 | 65,7 | 26,6 | 0,21× | perde |

**Leitura**: o fly é mais rápido que o Node e o Bun em **9 de 24** programas. Ganha com folga em `console_lines` (5,7×), `objects` (5,8×), `string` (2,3×), `dict_keys` (1,8×), `fib` (1,8×), `loop` (1,6×) e `exceptions` (1,6×). Ganha por pouco em `mandelbrot` (1,3×) e `array` (1,2×). `str_index` e `string_builder` ficam em volta de 1× e trocam de lado de uma rodada para outra.

Perde nos programas em que o tempo está em acessos a propriedades e elementos de arrays e em chamadas de função: `life` e `nbody_objects` ficam em 0,10× e 0,08× do melhor dos outros, `method_calls` em 0,11×, e `matrix`, `sieve`, `tak`, `closures` e `binary_trees` entre 0,21× e 0,35×. Pelo `callgrind`, o custo está nas buscas de propriedade e nas chamadas: cada acesso chama um auxiliar que copia e solta valores com contagem de referências, e cada chamada monta um quadro.

**O objetivo de ser mais rápido em qualquer situação ainda não foi atingido**: 15 das 24 cargas ficam atrás de pelo menos um dos dois. O próximo passo para essas cargas é um JIT que compile acessos a propriedades e chamadas para código nativo, com formas de objeto (um índice fixo por campo) e sem contagem de referências a cada leitura.

| | fly | Bun 1.4 | Node 22 |
| --- | ---: | ---: | ---: |
| tamanho do executável | **1,2 MB** | 107 MB | 123 MB |
| inicialização (`-e 'console.log(1)'`) | **3,5 ms** | 9 ms | 43 ms |

**Build com PGO**: `scripts/pgo-build.sh` treina o binário com os próprios benchmarks e fixtures e recompila com esse perfil. Precisa do componente `llvm-tools` do rustup (`rustup component add llvm-tools`). Nesta etapa o ganho foi pequeno e desigual: cerca de 6% a 13% no `array`, e nenhum ganho claro em `fib` e no laço. O `cargo build --release` comum já fica perto.

**Otimizações feitas**:
- **Acessos a elementos sem cópia**: `a[i]`, `m[i][j]` e as atribuições a eles, quando o array é uma variável local, leem e escrevem o array no próprio quadro, sem copiá-lo a cada acesso (`GetIndexLocal`, `GetIndex2Local`, `SetIndexLocal`, `SetIndex2Local`). Em `life`, `matrix` e `sieve`, medido antes e depois na mesma máquina: 2436→1983 ms, 115→98 ms e 673→526 ms.
- **Posição por nome**: cada função guarda, para cada nome de propriedade, a posição onde o último acesso o achou, e o código nativo confere essa posição antes de procurar. Pelo `callgrind`, `nbody_objects` (40 passos) passou de 301,6 M para 242,7 M instruções, e `method_calls` de 1699 M para 1505 M.
- **Índice por nome com hash rápido**: um objeto com mais de 16 propriedades, como `Math`, guarda o índice num hash simples por nome, em vez do SipHash, que custava mais que a busca.
- **Strings com contagem**: `JsStr` guarda o número de caracteres e se o texto é só ASCII, então `length` e `s[i]` custam o mesmo, sem percorrer o texto. Texto com caracteres fora do ASCII ainda percorre.
- **Alocador de blocos pequenos**: blocos de até 512 bytes saem de listas livres (`src/alloc.rs`), sem passar pelo malloc do sistema.
- **Condições como saltos**: `&&`, `||` e `!` em `if`, `while` e `for` viram saltos, sem empilhar booleanos.
- **Busca de método por nome**: os métodos de string e de array ficam numa tabela ordenada e são achados por busca binária.
- **Dados de classe só quando precisam**: uma função guarda os dados de classe (`ClassInfo`) só quando vira construtora ou recebe membros, e um objeto comum guarda só os campos que usa.

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
  └─ lexer.rs      tokens (com quebra de linha, para o ASI, e `_` nos números)
  └─ parser.rs     AST (ast.rs), descida recursiva + precedência. Desestruturação,
                   classes e `var` são traduzidos aqui para declarações, atribuições
                   e chamadas de %class, %items e %rest
  └─ compiler.rs   bytecode por função: resolve escopos, slots, upvalues,
                   células para variáveis capturadas, finally embutido
  └─ vm.rs         máquina de pilha, frames, exceções por tabela de handlers,
                   propriedades com cadeia de protótipos, event loop com timers e nextTick
  └─ value.rs      valores, objetos (propriedades, protótipo, acessores) e a busca na cadeia
  └─ collection.rs as entradas de Map e Set (ordem de inserção, SameValueZero)
  └─ jit/          código nativo x86-64 por função (codegen.rs), análise de
                   números por slot (types.rs), laços e funções de números em
                   registradores (region.rs), funções que o código nativo
                   chama (rt.rs), montador (x64.rs) e memória executável (mem.rs)
  └─ builtins/     console, globais, array, string, json, fs/path,
                   collections (Map e Set) e class (%class)
```

Pontos de projeto:

- **Módulos** são funções embrulhadas, como no CommonJS do Node: `exports`, `require`, `module`, `__filename`, `__dirname`.
- **Closures**: só as variáveis cujo nome é usado por uma função interna vão para células (`Rc<RefCell>`), que as funções internas compartilham. As demais ficam em slots da pilha, mesmo em funções que contêm closures.
- **Strings** são `Rc<String>`. Em `s += x` (e `s = s + x`) a string é estendida no lugar quando a referência é única, então montar uma string em laço é linear.
- **Código nativo**: a profundidade da pilha de operandos é conhecida em cada instrução, então cada operando tem um endereço fixo no quadro. O código nativo escreve diretamente só os valores sem referência. Para valores com referência, chama funções auxiliares que copiam, movem ou soltam o valor, e nos demais casos chama a própria VM, que usa o mesmo código do interpretador. O tamanho da pilha só é atualizado pelos helpers, que o definem antes de rodar.
- **Event loop**: `nextTick` roda antes de qualquer timer. Timers ficam numa heap de (prazo, id). Erros não tratados imprimem `Uncaught …` e saem com código 1.
- **Pilha de recursão**: recursão JavaScript pura estoura em `RangeError` (10 mil frames). Chamadas que passam por built-ins (como um callback de `map`) são limitadas a 2 mil, e o interpretador roda numa thread com 256 MB de pilha.
- **Objetos e protótipos**: um objeto tem, além das propriedades, uma referência ao protótipo e os acessores (getters e setters). A busca percorre a cadeia. Um getter é executado pela VM: o código nativo deixa esse caso para ela. Uma classe é uma chamada de `%class` que monta o construtor, o protótipo e os membros estáticos.

## Testes

```sh
cargo test --release   # os fixtures rodam no binário otimizado
cargo test             # e no de debug, com as asserções de limites ligadas
```

Os scripts em `tests/fixtures/*.fly` são executados duas vezes, com código nativo (o padrão) e no interpretador (`FLY_JIT=0`), e a saída é comparada com os arquivos `.out`. Os `.out` foram conferidos contra o **Node.js 22**: todos batem, exceto `fs_path` e `require_local`, que dependem de decisões próprias do fly (`Buffer` e resolução de `.fly`). Os fixtures `fast_paths`, `jit_numbers`, `jit_exceptions` e `native_paths` cobrem os caminhos otimizados: superinstruções com tipos mistos, zero negativo e `%` com sinal, NaN nas comparações, exceções que atravessam quadros nativos, recursão até o limite, closures, `switch`, e `try`/`finally` com `break` e `continue`. `jit_modulo` compara `%` por constante com `%` por divisor variável, inclusive o sinal do zero. `jit_loop_locals` muda o tipo de locais dentro de laços, chama funções e lança exceções. `scalar_objects` cobre a substituição de objetos. `native_calls` cobre chamadas diretas com a pilha crescendo, exceções que atravessam vários quadros, `push` com outros argumentos e callbacks com `this`.

Os fixtures `index_locals` e `property_hints` cobrem os acessos a elementos sem cópia e as posições guardadas por nome. `collections`, `destructuring`, `classes`, `error_classes`, `object_protos`, `objects_globals` e `delete_zero` comparam as funcionalidades novas com o Node, com e sem código nativo.

## Roadmap

1. **Chamadas ainda mais baratas**: o código nativo empurra e tira os quadros, busca upvalues sem uma função auxiliar e acessa os closures com layout fixo, em vez de contar referências a cada chamada. É o que falta para `fib` e os callbacks chegarem a 2×.
2. **Registradores no código nativo**: os locais de laços já ficam em XMM. Falta estender a funções inteiras, aos temporários e às constantes.
3. **Valores com NaN-boxing**, para que valores não tenham tamanho de 16 bytes nem contagem de referências nos números.
4. **Formas de objeto**: objetos com os mesmos campos, na mesma ordem, compartilhariam uma tabela de posições, e o código nativo leria o campo por índice, sem comparar nomes. Hoje a posição fica guardada por função e por nome.
5. **Promises, async/await, generators e Symbol**, que são as partes do Node moderno que faltam, além da zona morta temporal (TDZ) de `let` e `const`.
