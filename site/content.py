"""Conteúdo do site fly: página inicial e documentação.

Cada exemplo é executado pelo binário do fly durante o build (build.py), e a saída mostrada é a real.
Se um exemplo falhar, o build para. Os blocos são tuplas:

  ("h2", titulo, id)        uma seção, com âncora "<página>.<id>"
  ("h3", titulo)            subseção
  ("p", texto)              parágrafo; `código` e **negrito** são aceitos
  ("ul", [textos])          lista
  ("shell", texto)          comandos de terminal (não são executados)
  ("ex", dict)              exemplo executado: título, desc, file, code e opcionais
  ("note", rótulo, texto)   nota à parte
  ("table", cabeçalho, linhas)  tabela; células aceitam `código`
  ("probe", modo, linhas)   trechos executados mostrando o resultado; modo "fly" ou "both" (com o Node)
"""

HOME = {
    "lede": "A sintaxe do JavaScript e o modelo de execução do Node.js, num binário de 1,2 MB que abre em 3,5 ms.",
    "badges": ["versão 0.1.0", "sem dependências", "binário de 1,2 MB"],
    "resumo": [
        ("leve", "Um executável de 1,2 MB, sem dependências e sem node_modules. Cabe em um pendrive."),
        ("rápido", "Código nativo x86-64 para funções e laços com números, e callbacks de array em lote. Os números estão logo abaixo."),
        ("familiar", "let, const, arrow functions, template strings, spread, switch, try com finally e require. Quem conhece JavaScript lê o código sem aprender outra sintaxe."),
    ],
    "desempenho": "Os mesmos programas em cada runtime, com a mediana de 5 execuções intercaladas, em 4 vCPU. Barras menores são mais rápidas. Os tempos incluem o início de cada processo.",
    "como_ler": "De 24 programas, o fly é mais rápido que o Bun e o Node em 9. Nos outros 15 ainda perde, em geral nos programas com acessos a propriedades e elementos de arrays e chamadas de função. Diferenças menores que 10% dependem da máquina, e alguns casos perto de 1× trocam de lado de uma medição para outra.",
    "limites": [
        "Não há Promises, async/await, generators, expressões regulares nem Symbol.",
        "Strings são indexadas por caractere Unicode, não por unidade UTF-16. Por isso `\"😀\".length` é 1.",
        "A pilha de chamadas aguenta cerca de 10 mil quadros no interpretador.",
    ],
}

# Grupos da barra lateral da documentação, na ordem de leitura.
GROUPS = [
    ("Começar", ["docs-comecar"]),
    ("Linguagem", ["docs-sintaxe", "docs-controle", "docs-funcoes", "docs-objetos", "docs-texto", "docs-erros"]),
    ("Ambiente", ["docs-modulos", "docs-eventos", "docs-sistema"]),
    ("Referência", ["docs-referencia", "docs-diferencas", "docs-execucao"]),
]

PAGES = {}


def page(slug, title, lede, blocks, resumo):
    PAGES[slug] = {"slug": slug, "title": title, "lede": lede, "blocks": blocks, "resumo": resumo}


# ----------------------------------------------------------------- Começar
page(
    "docs-comecar", "Começar",
    "Compile o fly com o cargo, rode um arquivo e confira a saída. O projeto não tem dependências externas, então o único pré-requisito é o Rust.",
    [
        ("h2", "Instalar", "instalar"),
        ("p", "Você precisa do Rust instalado, com o cargo. Clone o repositório e compile o binário:"),
        ("shell", "git clone -b fly-language https://github.com/davixyzc/New-Project fly\ncd fly\ncargo build --release"),
        ("p", "O executável fica em `target/release/fly`. Ele tem menos de 1 MB e não precisa de nada instalado além do sistema."),
        ("h2", "Primeiro programa", "primeiro"),
        ("p", "Crie um arquivo com a extensão `.fly` e passe o caminho dele para o binário."),
        ("ex", {
            "title": "Olá, fly",
            "desc": "Um `console.log` aparece na saída padrão.",
            "file": "ola.fly",
            "code": 'console.log("Olá, fly!");',
        }),
        ("h2", "Rodar uma linha", "linha"),
        ("p", "Com `-e`, o fly roda o código que vem na linha de comando, sem criar arquivo."),
        ("ex", {
            "title": "Código na linha de comando",
            "desc": "`-e` aceita qualquer código entre aspas.",
            "eval": "console.log(2 ** 10)",
        }),
        ("h2", "Argumentos", "argumentos"),
        ("p", "Os valores depois do arquivo chegam em `process.argv`. O primeiro item é o runtime e o segundo é o caminho do script. Os argumentos começam no índice 2."),
        ("ex", {
            "title": "Argumentos do script",
            "desc": "Passe os nomes depois do arquivo e leia com `process.argv.slice(2)`.",
            "file": "argumentos.fly",
            "args": ["maria", "joao"],
            "code": 'console.log("argumentos:", process.argv.slice(2));',
        }),
        ("h2", "Opções", "opcoes"),
        ("table", ["opção", "o que faz"], [
            ["`-e, --eval <código>`", "roda o código que vem na linha de comando"],
            ["`-v, --version`", "mostra a versão"],
            ["`-h, --help`", "mostra a ajuda"],
            ["`FLY_JIT=0`", "variável de ambiente que desliga o código nativo (veja Como o fly executa)"],
        ]),
        ("p", "Quando um erro não é tratado, a mensagem vai para o erro padrão e o processo termina com código 1."),
    ],
    "Compilar, rodar um arquivo e usar a linha de comando.",
)

# ----------------------------------------------------------------- Sintaxe
page(
    "docs-sintaxe", "Sintaxe",
    "Variáveis, tipos e operadores. A sintaxe segue o JavaScript, e as partes que faltam estão em Diferenças.",
    [
        ("h2", "Variáveis", "variaveis"),
        ("p", "`const` cria uma variável que não pode ser reatribuída. `let` cria uma variável comum. `var` também existe, com escopo de bloco, igual a `let`."),
        ("ex", {
            "title": "let, const e var",
            "desc": "Reatribuição e escopo de bloco.",
            "file": "variaveis.fly",
            "code": 'const nome = "fly";\nlet versao = 1;\nversao += 1;\n\nif (versao > 1) {\n  var rotulo = "segunda versão";\n  console.log(nome, versao, rotulo);\n}',
        }),
        ("h2", "Desestruturação", "desestruturacao"),
        ("p", "Uma declaração pode tirar valores de um array ou de um objeto. Um padrão aceita valor padrão com `=`, um buraco com vírgula sobrando e `...resto` para o que sobrou."),
        ("ex", {
            "title": "Desestruturação",
            "desc": "Um array com um buraco e um padrão, e um objeto com um nome diferente.",
            "file": "desestruturacao.fly",
            "code": "const [primeiro, , terceiro = 0, ...resto] = [1, 2, undefined, 4, 5];\nconst { nome, idade: anos = 18 } = { nome: \"Ana\" };\nconsole.log(primeiro, terceiro, resto, nome, anos);",
        }),
        ("h2", "Tipos", "tipos"),
        ("p", "Os valores são number, string, boolean, undefined, null e objetos. Objetos incluem arrays e funções. Todo número é de ponto flutuante de 64 bits, como no JavaScript."),
        ("ex", {
            "title": "typeof",
            "desc": "`typeof` diz o tipo de um valor.",
            "file": "tipos.fly",
            "code": 'console.log(typeof 42, typeof "texto", typeof true);\nconsole.log(typeof undefined, typeof null, typeof []);\nconsole.log(typeof (() => 1), typeof NaN);',
        }),
        ("h2", "Operadores", "operadores"),
        ("p", "Os aritméticos são `+ - * / % **`. A comparação aceita `==` e `===`. Prefira `===`, que não converte tipos antes de comparar."),
        ("ex", {
            "title": "Operadores",
            "desc": "Potência, resto, comparação estrita e valor padrão com `??`.",
            "file": "operadores.fly",
            "code": 'console.log(2 ** 10, 7 % 3, -7 % 3, 10 / 4);\nconsole.log("3" === 3, "3" == 3, null ?? "padrão", 0 || "fallback");\nconsole.log(1 < 2 && 2 < 3, !0, 5 > 3 ? "maior" : "menor");',
        }),
        ("p", "`&&` e `||` param na primeira parte que decide e devolvem esse valor, e não um booleano. Por isso `0 || \"fallback\"` dá `fallback`."),
        ("p", "`??` troca só `null` e `undefined`. Por isso `0 ?? \"padrão\"` dá `0`."),
        ("p", "As atribuições compostas (`+=`, `-=`, `*=`, `/=`, `%=`, `**=`) e os operadores `++` e `--` funcionam como no JavaScript."),
    ],
    "Variáveis, tipos e operadores.",
)

page(
    "docs-controle", "Controle de fluxo",
    "Decisões com if, laços com for, while e do…while, e switch. Também mostra como pular uma volta ou sair de um laço.",
    [
        ("h2", "if e else", "if"),
        ("p", "A condição fica entre parênteses. Sem chaves, o if vale para uma única instrução."),
        ("ex", {
            "title": "Classificando notas",
            "desc": "Cada faixa devolve um texto.",
            "file": "notas.fly",
            "code": 'function faixa(nota) {\n  if (nota >= 9) return "excelente";\n  else if (nota >= 6) return "bom";\n  return "recuperação";\n}\n\nconsole.log([9.5, 7, 3].map(faixa));',
        }),
        ("h2", "Laços", "lacos"),
        ("ex", {
            "title": "for, while e do…while",
            "desc": "Os três laços que o fly tem.",
            "file": "lacos.fly",
            "code": 'for (let i = 1; i <= 3; i++) console.log("for", i);\n\nlet restam = 2;\nwhile (restam > 0) {\n  console.log("while", restam);\n  restam--;\n}\n\nlet tentativas = 0;\ndo {\n  tentativas++;\n} while (tentativas < 2);\nconsole.log("do…while rodou", tentativas, "vezes");',
        }),
        ("h2", "for…of e for…in", "for-of"),
        ("p", "`for…of` percorre os valores de um array. `for…in` percorre as chaves de um objeto."),
        ("ex", {
            "title": "for…of e for…in",
            "desc": "Valores de um array e chaves de um objeto.",
            "file": "percorrer.fly",
            "code": 'for (const cor of ["azul", "verde"]) console.log(cor);\n\nconst ponto = { x: 1, y: 2 };\nfor (const chave in ponto) console.log(chave, ponto[chave]);',
        }),
        ("h2", "break e continue", "break"),
        ("p", "`continue` passa para a próxima volta. `break` termina o laço."),
        ("ex", {
            "title": "Pular e sair",
            "desc": "Os números pares até 6.",
            "file": "pares.fly",
            "code": 'const pares = [];\nfor (let i = 0; i < 10; i++) {\n  if (i % 2 === 1) continue;\n  if (i > 6) break;\n  pares.push(i);\n}\nconsole.log(pares);',
        }),
        ("h2", "switch", "switch"),
        ("p", "Um caso sem `break` cai no próximo, como no JavaScript. Agrupar casos é o uso mais comum disso."),
        ("ex", {
            "title": "switch com casos agrupados",
            "desc": "`case 12`, `case 1` e `case 2` levam ao mesmo retorno.",
            "file": "estacoes.fly",
            "code": 'function estacao(mes) {\n  switch (mes) {\n    case 12:\n    case 1:\n    case 2:\n      return "verão";\n    case 3:\n      return "transição";\n    default:\n      return "outra";\n  }\n}\n\nconsole.log(estacao(1), estacao(3), estacao(7));',
        }),
    ],
    "if, laços, switch, break e continue.",
)

page(
    "docs-funcoes", "Funções",
    "Funções são valores: dá para guardar, passar para outra função e devolver. O fly tem declarações, expressões e arrow functions.",
    [
        ("h2", "Três formas de declarar", "formas"),
        ("ex", {
            "title": "Declaração, expressão e arrow",
            "desc": "As três formas dão o mesmo resultado aqui.",
            "file": "formas.fly",
            "code": 'function soma(a, b) {\n  return a + b;\n}\nconst subtrai = function (a, b) {\n  return a - b;\n};\nconst multiplica = (a, b) => a * b;\n\nconsole.log(soma(2, 3), subtrai(2, 3), multiplica(2, 3));',
        }),
        ("h2", "Parâmetros padrão e rest", "parametros"),
        ("p", "Um parâmetro com `=` tem valor padrão. Um parâmetro com `...` recebe os argumentos que sobram, em um array."),
        ("ex", {
            "title": "Padrão e rest",
            "desc": "Sem argumentos entra o padrão. Com mais de um, o resto vira array.",
            "file": "saudacao.fly",
            "code": 'function saudar(nome = "mundo", ...extras) {\n  const juntos = extras.length ? " e " + extras.join(", ") : "";\n  return `Olá, ${nome}${juntos}!`;\n}\n\nconsole.log(saudar());\nconsole.log(saudar("Ana", "Bruno", "Caio"));',
        }),
        ("h2", "Closures", "closures"),
        ("p", "Uma função guarda as variáveis do lugar onde foi criada. Enquanto ela existir, essas variáveis continuam vivas."),
        ("ex", {
            "title": "Contador com estado",
            "desc": "As duas funções devolvidas compartilham `total`.",
            "file": "contador.fly",
            "code": 'function contador() {\n  let total = 0;\n  return { inc: () => ++total, valor: () => total };\n}\n\nconst c = contador();\nc.inc();\nc.inc();\nconsole.log("valor:", c.valor());',
        }),
        ("ex", {
            "title": "Um let por iteração",
            "desc": "Cada volta do laço tem a sua própria variável, então cada função guarda o seu valor.",
            "file": "laco.fly",
            "code": 'const fns = [];\nfor (let i = 0; i < 3; i++) {\n  fns.push(() => i);\n}\nconsole.log(fns.map((f) => f()));',
        }),
        ("h2", "Recursão", "recursao"),
        ("ex", {
            "title": "Fatorial",
            "desc": "Uma função pode chamar a si mesma.",
            "file": "fatorial.fly",
            "code": "function fatorial(n) {\n  return n <= 1 ? 1 : n * fatorial(n - 1);\n}\n\nconsole.log(fatorial(10));",
        }),
        ("h2", "Métodos e this", "metodos"),
        ("p", "Dentro de um método, `this` é o objeto que recebeu a chamada. Devolver `this` permite encadear chamadas."),
        ("ex", {
            "title": "Método que devolve this",
            "desc": "Duas chamadas encadeadas no mesmo objeto.",
            "file": "conta.fly",
            "code": 'const conta = {\n  saldo: 100,\n  depositar(valor) {\n    this.saldo += valor;\n    return this;\n  },\n};\n\nconta.depositar(50).depositar(25);\nconsole.log(conta.saldo);',
        }),
        ("h2", "Funções construtoras", "construtoras"),
        ("p", "`new` funciona com funções comuns, que usam `this` para preencher o objeto novo. Classes têm sintaxe própria, logo abaixo."),
        ("ex", {
            "title": "new com função",
            "desc": "Uma função faz o papel de construtor.",
            "file": "ponto.fly",
            "code": "function Ponto(x, y) {\n  this.x = x;\n  this.y = y;\n}\n\nconst p = new Ponto(1, 2);\nconsole.log(p, p.x + p.y);",
        }),
        ("h2", "Classes", "classes"),
        ("p", "`class` agrupa o construtor e os métodos. `extends` herda da classe de cima, e `super` chama o método ou o construtor dela. Uma instância é impressa com o nome da classe."),
        ("ex", {
            "title": "Classe com herança",
            "desc": "Um método que chama o da classe de cima.",
            "file": "animais.fly",
            "code": "class Animal {\n  constructor(nome) {\n    this.nome = nome;\n  }\n  falar() {\n    return this.nome + \" faz um som\";\n  }\n}\n\nclass Cachorro extends Animal {\n  falar() {\n    return super.falar() + \" (au)\";\n  }\n}\n\nconst dog = new Cachorro(\"Rex\");\nconsole.log(dog.falar(), dog instanceof Animal);\nconsole.log(dog);",
        }),
    ],
    "Declarações, arrows, parâmetros, closures e recursão.",
)

page(
    "docs-objetos", "Objetos e arrays",
    "Objetos guardam pares de chave e valor. Arrays guardam listas ordenadas. Os dois funcionam bem com JSON.",
    [
        ("h2", "Objetos", "objetos"),
        ("p", "Um literal aceita o nome de uma variável como atalho: `{ x }` equivale a `{ x: x }`. Propriedades novas são criadas por atribuição."),
        ("ex", {
            "title": "Literais e atalhos",
            "desc": "Chaves, atalhos e uma propriedade adicionada depois.",
            "file": "objetos.fly",
            "code": 'const x = 1;\nconst ponto = { x, y: 2, rotulo: "A" };\nponto.z = 3;\nconsole.log(ponto, Object.keys(ponto));',
        }),
        ("h2", "Object.keys, values e entries", "object-metodos"),
        ("ex", {
            "title": "Ler um objeto",
            "desc": "As três funções devolvem arrays, na ordem em que as chaves foram criadas.",
            "file": "estoque.fly",
            "code": 'const estoque = { maçã: 4, pera: 0 };\nconsole.log(Object.keys(estoque));\nconsole.log(Object.values(estoque));\nconsole.log(Object.entries(estoque));',
        }),
        ("h2", "Arrays", "arrays"),
        ("p", "Arrays têm métodos de ordem superior, como `map`, `filter` e `reduce`. Eles devolvem arrays novos e não mudam o original."),
        ("ex", {
            "title": "map, filter, reduce e sort",
            "desc": "Spread junta arrays, e `sort` aceita um comparador.",
            "file": "arrays.fly",
            "code": 'const notas = [7.5, 9, 6];\nconst todas = [10, ...notas];\nconst media = notas.reduce((a, b) => a + b, 0) / notas.length;\n\nconsole.log({ media: media.toFixed(2), todas: todas.length });\nconsole.log(todas.filter((n) => n >= 7).sort((a, b) => b - a));',
        }),
        ("h2", "Map e Set", "map-set"),
        ("p", "`Map` guarda pares de chave e valor, e `Set` guarda valores sem repetição. As duas mantêm a ordem em que os itens entraram, e aceitam qualquer tipo como chave."),
        ("ex", {
            "title": "Contagem com Map e Set",
            "desc": "Um Map conta as letras, e um Set tira as repetições.",
            "file": "colecoes.fly",
            "code": "const contagem = new Map();\nfor (const letra of \"banana\") contagem.set(letra, (contagem.get(letra) || 0) + 1);\nconsole.log(contagem.get(\"a\"), contagem.size);\nfor (const [letra, n] of contagem) console.log(letra, n);\nconst unicos = new Set([3, 1, 3, 2, 1]);\nconsole.log(unicos, [...unicos].join(\" \"));",
        }),
        ("h2", "JSON", "json"),
        ("p", "`JSON.stringify` transforma um valor em texto, e `JSON.parse` faz o caminho contrário. O segundo argumento de `stringify` é ignorado, então passe `null`, como no exemplo, e use o terceiro para a indentação."),
        ("ex", {
            "title": "JSON com indentação",
            "desc": "Indentação de 2 espaços, e leitura do texto de volta.",
            "file": "json.fly",
            "code": 'const dados = { nome: "fly", versao: 0.1, tags: ["rápido", "leve"] };\nconst texto = JSON.stringify(dados, null, 2);\n\nconsole.log(texto);\nconsole.log(JSON.parse(texto).tags[1]);',
        }),
    ],
    "Literais, spread, métodos de array e JSON.",
)

page(
    "docs-texto", "Texto e números",
    "Strings com métodos e template strings, conversões entre tipos e a biblioteca Math.",
    [
        ("h2", "Strings", "strings"),
        ("p", "Strings não mudam: os métodos devolvem uma string nova. Em `slice`, índices negativos contam a partir do fim."),
        ("ex", {
            "title": "Métodos de string",
            "desc": "`slice` com índice negativo, `split` e `length`.",
            "file": "texto.fly",
            "code": 'const s = "Hello, World";\n\nconsole.log(s.toUpperCase(), s.slice(-5));\nconsole.log(s.split(", "));\nconsole.log(`${s.length} caracteres`);',
        }),
        ("h2", "Template strings", "templates"),
        ("p", "Uma string entre crases aceita expressões dentro de `${...}`."),
        ("ex", {
            "title": "Interpolação",
            "desc": "Qualquer expressão cabe dentro de `${...}`.",
            "file": "itens.fly",
            "code": 'const itens = ["pão", "leite", "café"];\nconsole.log(`Você tem ${itens.length} itens: ${itens.join(" e ")}.`);\nconsole.log(`Total: ${(12.5 * 2).toFixed(2)}`);',
        }),
        ("h2", "Conversões", "conversoes"),
        ("p", "`Number`, `String` e `Boolean` convertem valores. `parseInt` e `parseFloat` leem o começo de um texto e param no primeiro caractere que não faz parte do número."),
        ("ex", {
            "title": "Converter tipos",
            "desc": "Um texto que não é número vira NaN.",
            "file": "conversoes.fly",
            "code": 'console.log(Number("42") + 1, parseInt("12px"), parseFloat("3.5kg"));\nconsole.log(String(7) + 1, Boolean(""), Number("abc"));\nconsole.log(parseInt("ff", 16), isNaN("abc"), isFinite(1 / 0));',
        }),
        ("h2", "Números e Math", "math"),
        ("p", "`toFixed` devolve uma string com as casas decimais pedidas. `Math` tem arredondamento, potência, trigonometria e `random`."),
        ("ex", {
            "title": "Arredondar e calcular",
            "desc": "`Math.round(-2.5)` dá -2, como no JavaScript.",
            "file": "numeros.fly",
            "code": "console.log((0.1 + 0.2).toFixed(2), (2.5).toFixed(0));\nconsole.log(Math.round(2.5), Math.round(-2.5), Math.floor(-1.5));\nconsole.log(Math.max(1, 5, 3), Math.sqrt(16), Math.PI.toFixed(4));",
        }),
    ],
    "Strings, template strings, Math e conversões.",
)

page(
    "docs-erros", "Exceções",
    "throw lança um valor. try e catch tratam o erro. finally roda em qualquer caso, inclusive quando a função devolve um valor.",
    [
        ("h2", "throw, try e catch", "try"),
        ("ex", {
            "title": "Erros com nome e mensagem",
            "desc": "`catch` recebe o objeto de erro, com `name` e `message`.",
            "file": "erros.fly",
            "code": 'function dividir(a, b) {\n  if (b === 0) throw new RangeError("divisão por zero");\n  return a / b;\n}\n\ntry {\n  dividir(1, 0);\n} catch (e) {\n  console.log(e.name + ": " + e.message);\n}',
        }),
        ("p", "`throw` aceita qualquer valor. Um texto também serve, mas um objeto `Error` deixa claro o que aconteceu, e é o que este guia usa."),
        ("h2", "finally", "finally"),
        ("ex", {
            "title": "finally até no return",
            "desc": "O bloco roda quando o `try` termina com `return`, `break` ou `continue`.",
            "file": "finally.fly",
            "code": 'function ler() {\n  try {\n    return "conteúdo";\n  } finally {\n    console.log("fechando o arquivo");\n  }\n}\n\nconsole.log(ler());',
        }),
        ("h2", "Erros sem tratamento", "sem-tratamento"),
        ("p", "Um erro que nenhum `catch` pega é impresso no erro padrão, com o nome e a mensagem. O processo termina com código 1."),
        ("ex", {
            "title": "Erro não tratado",
            "desc": "O que chegou antes do erro sai normalmente. A mensagem vai para o erro padrão.",
            "eval": 'console.log("antes"); throw new Error("config inválida");',
            "expect": 1,
            "show_err": True,
            "node": False,
        }),
    ],
    "throw, try, catch e finally.",
)

# ----------------------------------------------------------------- Ambiente
page(
    "docs-modulos", "Módulos",
    "require carrega outro arquivo e devolve o que ele exportou. Cada arquivo roda uma vez. As chamadas seguintes recebem o mesmo objeto.",
    [
        ("h2", "exports e require", "exports"),
        ("p", "Um arquivo exporta valores com `exports.nome = valor` ou com `module.exports = valor`. Caminhos que começam com `./` ou `../` são relativos ao arquivo que chama `require`."),
        ("ex", {
            "title": "Módulo com área e perímetro",
            "desc": "`lib/geometria.fly` exporta funções, e `main.fly` as usa.",
            "file": "main.fly",
            "files": {
                "lib/geometria.fly": "exports.area = (r) => Math.PI * r * r;\nexports.perimetro = (r) => 2 * Math.PI * r;",
            },
            "code": 'const geo = require("./lib/geometria");\n\nconsole.log("área:", geo.area(2).toFixed(2));\nconsole.log("perímetro:", geo.perimetro(1).toFixed(2));',
            "node": False,
        }),
        ("h2", "Caminhos e pastas", "caminhos"),
        ("p", "Um arquivo `.fly` pode ser pedido sem extensão. Um arquivo `.js` precisa da extensão. Quando o caminho é uma pasta, o `index.fly` dela é carregado."),
        ("ex", {
            "title": "Pasta com index",
            "desc": "`require` de uma pasta carrega o `index.fly` que está dentro dela.",
            "file": "pasta.fly",
            "files": {"lib/util/index.fly": 'module.exports = { nome: "util-index" };'},
            "code": 'const util = require("./lib/util");\nconsole.log(util.nome);',
            "node": False,
        }),
        ("h2", "Cache", "cache"),
        ("ex", {
            "title": "Mesmo objeto",
            "desc": "Com e sem extensão, o caminho aponta para o mesmo arquivo. O cache devolve o mesmo objeto.",
            "file": "cache.fly",
            "files": {"lib/geometria.fly": "exports.area = (r) => Math.PI * r * r;"},
            "code": 'const a = require("./lib/geometria");\nconst b = require("./lib/geometria.fly");\nconsole.log(a === b);',
            "node": False,
        }),
        ("h2", "Módulos embutidos", "embutidos"),
        ("p", "`fs` e `path` também saem de `require`, sem instalar nada. `__dirname` e `__filename` dão a pasta e o caminho do arquivo que está rodando."),
        ("ex", {
            "title": "Embutidos e caminhos do script",
            "desc": "`path` e as variáveis do script.",
            "file": "embutidos.fly",
            "code": 'const path = require("path");\nconsole.log(path.basename("/dados/relatorio.csv"), path.extname("relatorio.csv"));\nconsole.log(typeof __dirname, typeof __filename);',
        }),
    ],
    "require, exports, caminhos, cache e módulos embutidos.",
)

page(
    "docs-eventos", "Event loop e timers",
    "Depois do código síncrono, o fly roda os callbacks nesta ordem: process.nextTick, setImmediate e, por último, os timers. É a mesma ordem de um script no Node.",
    [
        ("h2", "Ordem dos callbacks", "ordem"),
        ("p", "O código do arquivo roda primeiro, de cima para baixo. Depois, o event loop chama os callbacks que foram registrados."),
        ("ex", {
            "title": "Ordem de execução",
            "desc": "Síncrono, nextTick, setImmediate e um timer de 10 ms.",
            "file": "eventos.fly",
            "code": 'console.log("1. síncrono");\nsetTimeout(() => console.log("4. timer de 10 ms"), 10);\nsetImmediate(() => console.log("3. setImmediate"));\nprocess.nextTick(() => console.log("2. nextTick"));\nconsole.log("1b. fim do script");',
        }),
        ("h2", "setInterval e clearInterval", "intervalo"),
        ("p", "`setInterval` repete o callback a cada intervalo. `clearInterval` cancela, e pode ser chamado de dentro do próprio callback."),
        ("ex", {
            "title": "Repetir até cancelar",
            "desc": "O intervalo para depois da terceira volta.",
            "file": "intervalo.fly",
            "code": 'let n = 0;\nconst id = setInterval(() => {\n  n++;\n  console.log("tick", n);\n  if (n === 3) clearInterval(id);\n}, 5);',
        }),
        ("h2", "Argumentos extras", "argumentos-tick"),
        ("ex", {
            "title": "nextTick com argumentos",
            "desc": "`process.nextTick` aceita argumentos depois da função.",
            "file": "argumentos-tick.fly",
            "code": 'process.nextTick((a, b) => console.log("soma", a + b), 2, 3);\nconsole.log("antes");',
        }),
        ("p", "As operações de arquivo com callback, como `fs.readFile`, também chegam pelo event loop. Veja em Sistema."),
    ],
    "A ordem dos callbacks, setInterval e nextTick.",
)

page(
    "docs-sistema", "Sistema",
    "O que o fly oferece para falar com o sistema: a saída do console, as variáveis do processo, arquivos e caminhos.",
    [
        ("h2", "console", "console"),
        ("p", "`console.log`, `console.info` e `console.debug` escrevem na saída padrão. `console.error` e `console.warn` escrevem no erro padrão, então dá para separar resultado de aviso."),
        ("ex", {
            "title": "Saída e erro padrão",
            "desc": "Dois canais separados, que dá para redirecionar um de cada vez.",
            "file": "canais.fly",
            "code": 'console.log("para a saída padrão");\nconsole.error("para o erro padrão");',
            "show_err": True,
        }),
        ("h2", "process", "process"),
        ("p", "`process.argv`, `process.env`, `process.platform`, `process.version` e `process.cwd()` estão disponíveis. `process.exit(código)` termina o programa na hora."),
        ("ex", {
            "title": "process.exit",
            "desc": "O código depois de `process.exit` não roda.",
            "file": "saida.fly",
            "code": 'console.log("antes");\nprocess.exit(3);\nconsole.log("depois");',
            "expect": 3,
        }),
        ("h2", "fs", "fs"),
        ("p", "As funções `*Sync` esperam o resultado e devolvem na hora. `readFileSync` devolve sempre uma string. Os arquivos são criados na pasta atual."),
        ("ex", {
            "title": "Escrever e ler arquivos",
            "desc": "O exemplo apaga o arquivo que criou.",
            "file": "arquivos.fly",
            "code": 'const fs = require("fs");\n\nfs.writeFileSync("anotacoes.txt", "compras\\n");\nfs.appendFileSync("anotacoes.txt", "livros\\n");\n\nconst texto = fs.readFileSync("anotacoes.txt");\nconsole.log(texto.split("\\n").length - 1, "linhas");\nconsole.log(fs.existsSync("anotacoes.txt"));\nfs.unlinkSync("anotacoes.txt");',
        }),
        ("h2", "fs com callback", "fs-callback"),
        ("p", "Com callback, o `fs` faz a leitura na hora e entrega o resultado pelo event loop. O callback recebe `(erro, dados)`, e `erro` é `null` quando deu certo."),
        ("ex", {
            "title": "readFile com callback",
            "desc": "O código depois de `readFile` roda antes do callback.",
            "file": "leitura.fly",
            "files": {"notas.txt": "compras\nlivros\nviagem"},
            "code": 'const fs = require("fs");\n\nfs.readFile("notas.txt", (erro, texto) => {\n  if (erro) return console.log("não achei o arquivo");\n  console.log("li", texto.split("\\n").length, "linhas");\n});\nconsole.log("depois de readFile");',
        }),
        ("h2", "path", "path"),
        ("p", "`path` monta e separa caminhos. `path.join` resolve os `..` e junta as partes com `/`."),
        ("ex", {
            "title": "Montar e separar caminhos",
            "desc": "Os resultados não dependem da pasta em que o script roda.",
            "file": "caminhos.fly",
            "code": 'const path = require("path");\n\nconsole.log(path.join("pasta", "sub", "../arquivo.txt"));\nconsole.log(path.basename("/dados/relatorio.csv"), path.extname("relatorio.csv"));\nconsole.log(path.dirname("/dados/relatorio.csv"), path.sep);',
        }),
    ],
    "console, process, fs e path.",
)

# ----------------------------------------------------------------- Referência
page(
    "docs-referencia", "Referência",
    "Todas as funções embutidas do fly, com a assinatura e o que cada uma faz. Para ver o uso, consulte a página de cada assunto.",
    [
        ("h2", "Globais e conversões", "globais"),
        ("table", ["nome", "o que faz"], [
            ["`NaN`, `Infinity`", "valores numéricos especiais"],
            ["`Number(valor)`, `String(valor)`, `Boolean(valor)`", "convertem um valor para o tipo indicado"],
            ["`parseInt(texto, base?)`", "lê um inteiro do começo do texto"],
            ["`parseFloat(texto)`", "lê um número decimal do começo do texto"],
            ["`isNaN(valor)`, `isFinite(valor)`", "testam o valor como número"],
            ["`Math.PI`, `Math.E`", "constantes"],
        ]),
        ("h2", "console", "console-ref"),
        ("table", ["nome", "o que faz"], [
            ["`log(...)`, `info(...)`, `debug(...)`", "escrevem na saída padrão"],
            ["`error(...)`, `warn(...)`", "escrevem no erro padrão"],
        ]),
        ("h2", "process", "process-ref"),
        ("table", ["nome", "o que faz"], [
            ["`argv`", "array com o runtime, o script e os argumentos"],
            ["`env`, `platform`, `version`", "variáveis de ambiente, nome da plataforma e versão"],
            ["`cwd()`", "pasta atual"],
            ["`exit(código)`", "termina o programa com o código indicado"],
            ["`nextTick(função, ...argumentos)`", "agenda a função para depois do código síncrono"],
        ]),
        ("h2", "Timers", "timers"),
        ("table", ["nome", "o que faz"], [
            ["`setTimeout(função, ms)`", "chama a função depois de `ms` milissegundos"],
            ["`setInterval(função, ms)`", "chama a função a cada `ms` milissegundos"],
            ["`setImmediate(função)`", "chama a função depois dos callbacks de I/O"],
            ["`clearTimeout(id)`, `clearInterval(id)`, `clearImmediate(id)`", "cancelam o agendamento"],
        ]),
        ("h2", "require e caminhos do script", "require-ref"),
        ("table", ["nome", "o que faz"], [
            ["`require(caminho)`", "carrega um arquivo `.fly` (sem extensão ou com ela), um `.js` (com a extensão) ou uma pasta com `index.fly`"],
            ["`__dirname`, `__filename`", "pasta e caminho do arquivo que está rodando"],
        ]),
        ("h2", "fs", "fs-ref"),
        ("table", ["nome", "o que faz"], [
            ["`readFileSync(caminho)`", "devolve o conteúdo como string"],
            ["`writeFileSync(caminho, texto)`, `appendFileSync(caminho, texto)`", "escrevem, substituindo ou acrescentando"],
            ["`existsSync(caminho)`", "diz se o caminho existe"],
            ["`readdirSync(pasta)`", "devolve os nomes dos itens da pasta"],
            ["`mkdirSync(pasta)`, `unlinkSync(caminho)`", "criam uma pasta e apagam um arquivo"],
            ["`readFile(caminho, cb)`", "lê o arquivo e chama `cb(erro, dados)`"],
            ["`writeFile(caminho, texto, cb)`", "escreve o arquivo e chama `cb(erro)`"],
        ]),
        ("h2", "path", "path-ref"),
        ("table", ["nome", "o que faz"], [
            ["`join(...partes)`, `resolve(...partes)`", "montam um caminho; `resolve` devolve um caminho absoluto"],
            ["`basename(caminho)`, `dirname(caminho)`, `extname(caminho)`", "nome do arquivo, pasta e extensão"],
            ["`sep`", "separador de caminhos, `/`"],
        ]),
        ("h2", "JSON, Object e Date", "json-ref"),
        ("table", ["nome", "o que faz"], [
            ["`JSON.stringify(valor, _, indentação)`", "transforma o valor em texto; o segundo argumento é ignorado"],
            ["`JSON.parse(texto)`", "lê um texto JSON"],
            ["`Object.keys(obj)`, `Object.values(obj)`, `Object.entries(obj)`", "arrays com as chaves, os valores ou os pares"],
            ["`Date.now()`", "milissegundos desde 1970"],
        ]),
        ("h2", "Math", "math-ref"),
        ("table", ["nome", "o que faz"], [
            ["`floor`, `ceil`, `round`, `trunc`", "arredondamentos"],
            ["`abs`, `sign`, `sqrt`, `pow`", "valor absoluto, sinal, raiz e potência"],
            ["`min`, `max`", "menor e maior valor entre os argumentos"],
            ["`log`, `exp`", "logaritmo natural e exponencial"],
            ["`sin`, `cos`, `tan`", "trigonometria, em radianos"],
            ["`random()`", "número aleatório entre 0 e 1"],
        ]),
        ("h2", "Erros", "erros-ref"),
        ("table", ["nome", "o que faz"], [
            ["`Error(mensagem)`", "erro genérico, com `name` igual a `Error`"],
            ["`TypeError`, `RangeError`, `SyntaxError`, `ReferenceError`", "erros específicos, com o mesmo formato"],
        ]),
        ("h2", "Métodos de array", "metodos-array"),
        ("table", ["método", "o que faz"], [
            ["`push`, `pop`, `shift`, `unshift`", "adicionam e removem itens nas pontas"],
            ["`slice`, `concat`, `reverse`", "devolvem um array novo, ou invertem o atual"],
            ["`indexOf`, `includes`, `find`, `findIndex`", "procuram um valor ou uma posição"],
            ["`map`, `filter`, `forEach`, `reduce`", "percorrem o array com um callback"],
            ["`some`, `every`", "testam se algum ou todos os itens passam"],
            ["`join(separador)`", "junta os itens em um texto"],
            ["`sort(comparador?)`", "ordena; sem comparador, compara como texto"],
        ]),
        ("h2", "Métodos de string", "metodos-string"),
        ("table", ["método", "o que faz"], [
            ["`length`, `charAt`, `charCodeAt`", "tamanho, caractere e código de um caractere"],
            ["`indexOf`, `includes`, `startsWith`, `endsWith`", "procuram um texto"],
            ["`slice`, `substring`", "recortam o texto; `slice` aceita índices negativos"],
            ["`toUpperCase`, `toLowerCase`, `trim`, `trimStart`, `trimEnd`", "mudam caixa e espaços"],
            ["`split`, `repeat`, `padStart`, `padEnd`, `concat`", "montam e separam textos"],
            ["`replace(padrão, novo)`", "troca a primeira ocorrência de um texto; não aceita regex"],
        ]),
        ("h2", "Métodos de number", "metodos-number"),
        ("table", ["método", "o que faz"], [
            ["`toFixed(casas)`", "devolve uma string com as casas decimais pedidas"],
            ["`toString(base)`", "devolve o número como string; a base vai de 2 a 36"],
        ]),
        ("h2", "Map e Set", "map-set"),
        ("table", ["método", "o que faz"], [
            ["`get(chave)`, `set(chave, valor)`", "lê e grava um valor. `set` devolve a própria coleção"],
            ["`has(chave)`, `delete(chave)`", "diz se a chave existe, e remove a entrada (devolve `true` se havia)"],
            ["`add(valor)`", "só no `Set`: adiciona um valor e devolve o próprio conjunto"],
            ["`clear()`", "remove todas as entradas"],
            ["`size`", "número de entradas (é uma propriedade, não um método)"],
            ["`forEach(função)`", "chama a função com `(valor, chave, coleção)` em ordem de inserção"],
            ["`keys()`, `values()`, `entries()`", "devolvem arrays com as chaves, os valores ou os pares `[chave, valor]`"],
        ]),
    ],
    "Todas as funções e métodos embutidos.",
)

page(
    "docs-diferencas", "Diferenças do JavaScript",
    "O fly segue a sintaxe e o modelo do Node, mas ainda não cobre tudo. Esta página lista o que falta e o que se comporta de outro jeito. Os resultados são os do binário.",
    [
        ("h2", "Sintaxe e recursos que não existem", "faltam"),
        ("p", "Cada trecho abaixo dá erro no fly. A mensagem mostrada é a que o binário imprime."),
        ("probe", "fly", [
            "class A { #x = 1; }",
            "class A { static { } }",
            "async function f() {}",
            '/a/.test("a");',
            "outer: for (;;) break outer;",
            "const o = { get x() { return 1; } };",
            "const n = null; console.log(n?.valor);",
            "new Array(3);",
            'Symbol("x");',
            "function f() { return arguments.length; } f(1);",
            "function* g() { yield 1; }",
        ]),
        ("h2", "Comportamento diferente", "diferente"),
        ("p", "Estes trechos rodam nos dois, mas dão resultados diferentes."),
        ("probe", "both", [
            'console.log("😀".length);',
            'console.log(JSON.stringify({ a: 1, b: 2 }, ["a"]));',
            "console.log(typeof Date);",
        ]),
        ("h2", "Outras diferenças", "outras"),
        ("ul", [
            "Um `let` ou `const` lido antes da declaração devolve `undefined`. Não há zona morta temporal, então não há erro.",
            "Uma `class` pode ser chamada sem `new`, sem erro, e `this` antes de `super()` não é verificado.",
            "`Object.getPrototypeOf` de um objeto comum devolve `null`, porque não há `Object.prototype`. Uma classe não pode estender `Map` nem `Set`.",
            "`for…of` e o spread de `Map` e `Set` percorrem uma cópia. `keys()`, `values()` e `entries()` devolvem arrays, sem `next()`.",
            "Strings são indexadas por caractere Unicode. O emoji conta como 1 caractere.",
            "`console.log` mostra objetos e arrays numa linha só, mesmo quando ficam longos. Os níveis além do segundo viram `[Array]` ou `[Object]`, como no Node.",
            "`readFileSync` devolve sempre uma string. As funções com callback do `fs` fazem o I/O na hora e entregam o resultado pelo event loop.",
            "`require` aceita `.fly` sem extensão. Um `.js` precisa da extensão. Não existe `node_modules`.",
            "`toFixed` pode arredondar de forma diferente do V8 em alguns valores de ponto flutuante.",
            "A pilha de chamadas aguenta cerca de 10 mil quadros no interpretador. Passando disso, a execução para com `RangeError: Maximum call stack size exceeded`. Uma função só de números pode ir mais fundo, porque roda em código nativo.",
            "`JSON.stringify` ignora o segundo argumento.",
            "`Math`, `Object`, `Array` e `Date` não têm todas as funções do Node. As listadas em Referência são as que existem.",
        ]),
    ],
    "O que falta em relação ao JavaScript, e o que se comporta diferente.",
)

page(
    "docs-execucao", "Como o fly executa",
    "O fly compila o código para bytecode e o roda numa máquina de pilha. Funções e laços que esquentam viram código nativo x86-64, gerado na primeira vez que rodam.",
    [
        ("h2", "Interpretador", "interpretador"),
        ("p", "O código passa pelo lexer e pelo parser, e o compiler gera bytecode por função. A máquina virtual executa esse bytecode com uma pilha de valores. O event loop cuida dos timers e dos callbacks."),
        ("h2", "Código nativo", "nativo"),
        ("p", "Na primeira chamada, cada função é traduzida para instruções do processador. O código nativo trabalha sobre a mesma pilha do interpretador. Operações com objetos e strings chamam a máquina virtual, então os dois caminhos convivem."),
        ("h2", "Laços e números", "numeros"),
        ("p", "Dentro de um laço, os locais que guardam números ficam em registradores. Dividir por uma constante usa multiplicação mágica. Quando o contador sobe de um em um, o resto dele fica num registrador e sobe junto."),
        ("h2", "Funções de números", "funcoes-numeros"),
        ("p", "Uma função que só faz contas com números roda direto em código nativo, e as chamadas entre elas também. A recursão usa a pilha do processador, com limite de 1 MB. Quando essa pilha acaba, a chamada volta para o interpretador."),
        ("h2", "Callbacks de array", "callbacks"),
        ("p", "`map`, `filter` e `reduce` com um callback de números rodam em lote, em código nativo. O lote para no primeiro valor que não é número, e o interpretador continua dali."),
        ("h2", "Desligar o código nativo", "desligar"),
        ("p", "Com `FLY_JIT=0`, o fly roda só o interpretador. Use isso para comparar os dois caminhos ou para depurar um resultado suspeito."),
        ("shell", "FLY_JIT=0 ./target/release/fly arquivo.fly"),
        ("p", "Os números de desempenho estão na página inicial, com o método de medição."),
    ],
    "Interpretador, código nativo e limites de execução.",
)

# Os tópicos da página inicial, na ordem da documentação.
TOPICS = [(slug, PAGES[slug]["title"], PAGES[slug]["resumo"]) for _, slugs in GROUPS for slug in slugs]
