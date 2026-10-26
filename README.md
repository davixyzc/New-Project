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

**A máquina varia bastante, até dentro de uma mesma sessão**: entre duas rodadas, o Node e o Bun chegaram a medir quase 2× mais rápido, e os casos perto de 1× trocam de lado de uma rodada para outra. Compare as colunas de uma mesma tabela, nunca tabelas de medições diferentes. Acima de 1× o fly ganha; abaixo, perde.

| benchmark | fly ms | Node ms | Bun ms | fly em relação ao melhor dos outros | resultado |
| --- | ---: | ---: | ---: | ---: | --- |
| `array` | 49,4 | 112,5 | 60,5 | 1,22× | fly ganha |
| `binary_trees` | 63,4 | 56,2 | 56,3 | 0,89× | perde |
| `char_codes` | 42,3 | 61,1 | 46,1 | 1,09× | fly ganha |
| `closures` | 84,2 | 39,8 | 62,2 | 0,47× | perde |
| `console_lines` | 20,7 | 549,8 | 89,6 | 4,32× | fly ganha |
| `dict_keys` | 14,4 | 57,4 | 25,0 | 1,74× | fly ganha |
| `exceptions` | 33,5 | 328,2 | 68,3 | 2,04× | fly ganha |
| `fib` | 10,2 | 42,3 | 15,8 | 1,55× | fly ganha |
| `json` | 120,6 | 183,1 | 79,8 | 0,66× | perde |
| `life` | 486,7 | 207,4 | 167,5 | 0,34× | perde |
| `loop` | 32,8 | 70,6 | 40,4 | 1,23× | fly ganha |
| `mandelbrot` | 16,6 | 54,3 | 19,7 | 1,18× | fly ganha |
| `matrix` | 22,2 | 44,6 | 24,4 | 1,10× | fly ganha |
| `method_calls` | 69,6 | 40,3 | 15,9 | 0,23× | perde |
| `nbody_objects` | 108,6 | 48,9 | 23,0 | 0,21× | perde |
| `num_to_string` | 44,0 | 74,7 | 90,7 | 1,70× | fly ganha |
| `objects` | 4,7 | 38,6 | 25,0 | 5,36× | fly ganha |
| `sieve` | 167,0 | 161,9 | 89,1 | 0,53× | perde |
| `sort_cmp` | 61,0 | 135,8 | 85,2 | 1,40× | fly ganha |
| `split_join` | 52,8 | 93,5 | 52,7 | 1,00× | perde |
| `str_index` | 9,7 | 41,1 | 12,0 | 1,24× | fly ganha |
| `string` | 9,9 | 61,1 | 22,2 | 2,25× | fly ganha |
| `string_builder` | 56,4 | 118,9 | 79,9 | 1,42× | fly ganha |
| `tak` | 11,0 | 45,4 | 18,7 | 1,70× | fly ganha |

**Leitura**: o fly é mais rápido que o Node e o Bun em **16 de 24** programas. Ganha com folga em `objects` (5,36×), `console_lines` (4,32×), `string` (2,25×), `exceptions` (2,04×), `dict_keys` (1,74×), `num_to_string` (1,70×), `tak` (1,70×), `fib` (1,55×), `string_builder` (1,42×) e `sort_cmp` (1,40×). Ganha por pouco em `loop` (1,23×), `str_index` (1,24×), `array` (1,22×), `mandelbrot` (1,18×), `matrix` (1,10×) e `char_codes` (1,09×).

Perde nos programas em que o tempo está em chamadas de função e em acessos a propriedades de objetos: `nbody_objects` (0,21×), `method_calls` (0,23×), `life` (0,34×), `closures` (0,47×), `sieve` (0,53×), `json` (0,66×) e `binary_trees` (0,89×). `split_join` (1,00×) fica empatado.

**O objetivo de ser mais rápido em qualquer situação ainda não foi atingido**: 8 das 24 cargas ficam atrás de pelo menos um dos dois. As medições desta rodada mostram onde está o gargalo, e ele está na forma como o código roda, não num auxiliar específico:
- Em `tak`, antes de compilar as funções com expressões mais fundas, 65% das instruções estavam em chamada e retorno (`callgrind`). Agora `tak` roda em código nativo de números e ficou à frente.
- Em `nbody_objects`, cerca de um terço das instruções está em leituras e escritas de propriedade: a busca, a cópia do valor e a liberação dele.
- Em `method_calls`, cada iteração tem cerca de 380 instruções, somando a chamada, os acessos a propriedade e o código gerado para as operações.
- Ajustes no caminho geral de chamadas mudaram menos de 0,3% das instruções. O custo está espalhado em muitos passos pequenos, e só cai com um quadro de chamada montado pelo próprio código nativo.

Para chegar a 2× ou mais nessas cargas, o código nativo precisa ler elementos e propriedades direto da memória. Isso pede um layout estável para arrays e objetos, que hoje são `Vec` e `RefCell`, sem layout definido. Ver o roadmap.

| | fly | Bun 1.4 | Node 22 |
| --- | ---: | ---: | ---: |
| tamanho do executável | **1,2 MB** | 107 MB | 123 MB |
| inicialização (`-e 'console.log(1)'`) | **2,7 ms** | 6 ms | 34 ms |

**Build com PGO**: `scripts/pgo-build.sh` treina o binário com os próprios benchmarks e fixtures e recompila com esse perfil. Precisa do componente `llvm-tools` do rustup (`rustup component add llvm-tools`). Numa medição feita antes das otimizações desta rodada, o ganho foi pequeno e desigual: cerca de 6% a 13% no `array`, e nenhum ganho claro em `fib` e no laço. O `cargo build --release` comum já fica perto. Não foi medido de novo nesta rodada.

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
- **Escalares sem `drop`**: sobrescrever ou descartar um número não chama o destrutor do valor, nem nos auxiliares de índice e de `%`. Em `life`, com 20 gerações, isso tirou cerca de 6% das instruções (1484 M para 1391 M, pelo `callgrind`).
- **Funções de números com expressões mais fundas**: a pilha de operandos de uma função de números vai até a profundidade 13, em `xmm2` a `xmm15`, porque os locais de uma função ficam no quadro, não em registradores. Antes, o limite de 7 deixava `tak` (chamadas aninhadas nos argumentos, profundidade 9) no interpretador. Pelo `callgrind`, `tak` foi de 924 M para 78 M instruções (−92%).
- **Concatenação no código nativo**: um `+` com string e valores primitivos roda no auxiliar `rt::concat_top`, que estende a string no lugar quando ela não é compartilhada. Objetos seguem pelo caminho da instrução, porque convertem com código do usuário. Números entram direto, sem `String` temporária (`int_text`). Pelo `callgrind`, `string_builder` foi de 410 M para 263 M instruções (−36%), e `num_to_string`, de 1162 M para 1082 M (−7%).
- **Busca de método com a posição do acesso**: a leitura de um método de string ou de array guarda a posição que achou, e a próxima leitura do mesmo acesso confere o nome nessa posição antes de procurar (`sorted_lookup_hinted`). Pelo `callgrind`, `char_codes` foi de 996 M para 766 M instruções (−23%).
- **JSON sem cópias**: `JSON.stringify` lê arrays e objetos onde estão, escreve cada membro direto na saída e formata números sem `String` temporária, e `JSON.parse` entrega as strings sem copiá-las. Pelo `callgrind`, `json` foi de 1533 M para 1040 M instruções (−32%).
- **`sort` com comparador numérico**: quando o comparador é compilado para números e os itens também são, o `sort` chama a função nativa direto, sem o caminho de callback a cada comparação. Pelo `callgrind`, `sort_cmp` foi de 1148 M para 807 M instruções (−30%).
- **Globais por índice**: cada global guarda o slot onde o nome foi achado, e a próxima leitura ou escrita confere esse slot antes de procurar. Nenhum global é removido, então um slot nunca muda de nome. Pelo `callgrind`, quatro milhões de leituras de `Infinity` e `NaN` foram de 725 M para 405 M instruções (−44%). Um `console.log` em loop perdeu 5% das instruções, e `nbody_objects`, 3%. Em tempo de parede, a leitura de globais ficou 32% mais rápida, e `console_lines`, cerca de 7%.
- **Ordenação sobre `f64`**: quando todos os itens são números e o comparador roda como código de número, o `sort` ordena um vetor de `f64`, sem criar um `Value` por comparação. Um comparador que devolve NaN conta como +0, como manda a especificação. Pelo `callgrind`, `sort_cmp` foi de 807 M para 364 M instruções (−55%), também porque o `%` de inteiros até 2^63 deixou de chamar o `fmod`.
- **Métodos embutidos no código nativo**: `charCodeAt` em string, `String(n)` com número e `.length` de string rodam direto, sem a chamada geral, como o `push` de arrays. Pelo `callgrind`, `char_codes` foi de 766 M para 392 M instruções (−49%).
- **`x += e` com números no código nativo**: antes, cada `t += String(n).length` caía no interpretador. Agora a soma é feita no código nativo, e só strings e células seguem o caminho geral. `num_to_string` foi de 997 M para 458 M instruções (−54%).
- **Inteiros com dois dígitos por passo**: a formatação de inteiros divide por 100 e não por 10. Caiu de cerca de 123 para 43 instruções por número.

Numa rodada completa feita depois dessas mudanças, 12 de 24 programas ficaram à frente de Node e Bun. A tabela acima é da rodada anterior, e nesta máquina as medidas de tempo variam bastante de uma execução para outra.

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
- **Closures**: só as variáveis cujo nome é usado por uma função interna vão para células (`Rc<RefCell>`), que as funções internas compartilham. As demais ficam em slots da pilha, mesmo em funções que contêm closures. Um parâmetro que uma função interna usa, e que nada na função escreve, não vai para célula: a função interna guarda uma cópia do valor, feita quando ela é criada. Pelo `callgrind`, `closures` foi de 717 M para 659 M instruções.
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

Os fixtures `deep_calls` (funções de números com a pilha de operandos mais funda, em chamadas aninhadas), `concat_forms` (`+` com string, números, valores especiais, objetos e cópias que não podem mudar), `json_forms` (escapes, aninhamento, indentação, ordem das chaves e valores omitidos) e `sort_comparators` (comparadores numéricos e não numéricos) também comparam com o Node, com e sem código nativo.

## Roadmap

1. **Quadros de chamada no código nativo**: o código nativo empurra e tira o quadro sozinho, sobre uma pilha de valores com layout fixo. Hoje uma chamada geral custa cerca de 250 instruções, espalhadas em muitos passos pequenos. É o que falta para `method_calls`, `closures` e `binary_trees`.
2. **Layout estável para arrays e objetos**: `Vec` e `RefCell` não têm layout definido, então o código nativo não lê um elemento ou uma propriedade direto da memória. Um tipo próprio, com ponteiro e tamanho, permitiria ler `a[i]` e `o.campo` no código nativo, o que pede `life`, `matrix` e `sieve`.
3. **Formas de objeto**: objetos com os mesmos campos, na mesma ordem, compartilhariam uma tabela de posições, e o código nativo leria o campo por índice. Sozinho ganha pouco, porque a busca por nome com posição guardada já é barata, e o custo maior está no auxiliar que a chama. Faz sentido depois do item 2.
4. **Laços com acesso a elementos em registradores**: um laço que chama `a[i]` cai hoje no código geral, com os locais na memória. Manter os números em XMM e salvá-los só em volta da chamada do auxiliar é o próximo passo para `life` e `matrix`.
5. **Registradores no código nativo**: estender os locais de laço às funções inteiras, aos temporários e às constantes.
6. **Valores com NaN-boxing**, para que valores não tenham 16 bytes nem contagem de referências nos números.
7. **Promises, async/await, generators e Symbol**, que são as partes do Node moderno que faltam, além da zona morta temporal (TDZ) de `let` e `const`.
