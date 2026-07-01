# Surface 标准化

Wikispine 会把 Wikipedia / Wikidata 中抽取出的 surface key 编译进自动机，并用同一套规则标准化用户查询文本。builder 和 runtime 必须使用完全一致的标准化契约；一个 runtime 数据包只对实现了这套契约的 runtime 有效。

标准化的目标是提高召回率。Wikipedia 标题、重定向、label、alias 以及用户输入文本之间，经常只差大小写、全半角、不可见格式字符、分隔符、重音符号或前后标点。Wikispine 会把这些差异收敛到同一个 surface key，再用于自动机编译和查询。

## 输出形态

标准化后的文本是 Unicode 字符串，并满足以下约束：

- 只使用 ASCII 空格 U+0020 表示空白。
- 连续空格会被压缩成一个空格。
- 前后空格会被裁掉。
- 字母大小写使用完整 Unicode case folding 折叠。
- 全角、半角和兼容形式通过 NFKC 收敛到普通形式。
- 组合音标会被删除。
- 零宽字符和 default-ignorable 格式字符会被删除。

如果结果为空，builder preprocess 阶段会丢弃这个 surface。

## 算法

按以下顺序处理：

1. 将空白类字符和分隔符类字符替换为 ASCII 空格。
2. 删除零宽字符和 default-ignorable 字符。
3. 执行 Unicode NFKC 兼容规范化。
4. 使用 non-Turkic 默认映射执行完整 Unicode case folding。
5. 执行 Unicode NFD 分解，并删除组合音标。
6. 将规范化过程中产生的空白类字符和分隔符类字符继续替换为 ASCII 空格。
7. 将连续空格压缩成一个空格。
8. 裁掉前后空格。

实现可以用流式 transducer 完成这些步骤，只要最终输出完全一致即可。

## 删除字符

以下字符不应影响匹配，直接删除：

- zero width space、zero width non-joiner、zero width joiner、zero width no-break space
- word joiner 和不可见分隔控制字符
- byte order mark
- soft hyphen
- variation selector
- 双向文本格式控制字符
- 其他 Unicode default-ignorable 格式字符

## 空白字符

以下字符统一转成 ASCII 空格：

- 所有 Unicode whitespace
- 所有 Unicode space separator、line separator、paragraph separator
- tab、carriage return、newline
- non-breaking space、narrow no-break space、ideographic space、em space、en space、thin space、hair space 等

## 可见分隔符

以下可见标点按分隔符处理，统一转成 ASCII 空格：

- 下划线
- 各种 dash 和 hyphen
- slash 和 backslash
- pipe
- 句号、逗号、冒号、分号、感叹号、问号
- 引号
- 括号
- 中文、日文标点
- middle dot、bullet、katakana middle dot

`+`、`#`、`&` 会被保留，因为它们在 `C++`、`C#`、`R&B` 这类 surface 中承载实体含义。

## 示例

标准化不是简单的字符替换。Unicode scalar value、UTF-8 字节数、UTF-16 code unit 数量都可能变化。客户端实现不能假设标准化后的字符串和原始字符串拥有相同长度或相同 offset。

| 输入 | 标准化后 | 变化说明 |
| --- | --- | --- |
| `Wikipedia_Title` | `wikipedia title` | 下划线变空格，大小写被折叠 |
| `Ｗｉｋｉｐｅｄｉａ` | `wikipedia` | 全角字母收敛成普通 ASCII |
| `Café` | `cafe` | 重音符号被删除 |
| `Straße` | `strasse` | 完整 case folding 会把 `ß` 展开成 `ss`，字符数增加 |
| `İstanbul` | `istanbul` | case folding 产生组合音标，随后组合音标被删除 |
| `Alan​Turing` | `alanturing` | 零宽字符被删除，字符数减少 |
| `Jean‑Paul Sartre` | `jean paul sartre` | 非 ASCII hyphen 变空格 |
| `A---B` | `a b` | 连续分隔符压缩成一个空格，字符数减少 |
| `① theorem` | `1 theorem` | NFKC 将兼容符号收敛成数字 |
| `《北京大学》` | `北京大学` | 前后标点被裁掉 |
| `西格蒙德·弗洛伊德` | `西格蒙德 弗洛伊德` | middle dot 变空格 |
| `C++` | `c++` | `+` 被保留 |
| `C#` | `c#` | `#` 被保留 |
| `R&B` | `r&b` | `&` 被保留 |

这些长度变化是 runtime 必须报告原文 offset 的原因。自动机运行在标准化文本上，但用户和下游系统需要的是原始输入文本中的位置。

## 匹配 Offset

runtime 查询会在标准化流上执行搜索，但输出的 `start` 和 `end` 是原始输入文本的 UTF-16 offset。runtime 实现必须在扫描时维护 normalized UTF-16 position 到 original UTF-16 position 的映射。
