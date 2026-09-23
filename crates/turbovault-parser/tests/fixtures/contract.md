<!--
Fixture for tests/test_contract_snapshot.rs. Every construct that has broken in
this parser, plus enough of the rest to notice collateral damage.

No frontmatter: `parse_blocks` takes a note's body, and callers strip the
frontmatter with `extract_frontmatter` first. Feeding it a `---` block makes it
read the delimiter as a horizontal rule and the keys as a setext heading.
-->

# Heading with ![alt-in-heading](heading.png) an image

## Heading with [link-in-heading](https://example.com/heading) a link

### Heading with [![badge-in-heading](badge.png)](https://ci.example) a linked image

#### ![banner-only-heading](banner.png)

A plain paragraph with **strong**, *emphasis*, ~~strikethrough~~ and `inline code`.

A paragraph with ![bare-img](bare.png), [a link](https://example.com/one), a
[![linked-img](linked.png)](https://example.com/two) badge, an
![titled-img](titled.png "The Title") titled image and an
![spaced-img](<spaced path.png>) spaced destination.

A paragraph with a [[wikilink]] and a [[wikilink|piped alias]].

```rust
fn top_level() {
    println!("fence at the top level");
}
```

    an indented code block

- tight one
- tight two
- tight three

1. ordered one
2. ordered two

5. starting at five
6. and six

- [x] a finished task
- [ ] an unfinished task

- outer item
  - nested item
    - deeper item
- second outer

- [ ] outer task
  - [x] nested task

1. outer ordered
   1. nested ordered

- loose one

- loose two

- item with an ![img-in-item](item.png) image
- item with a [link-in-item](https://example.com/item) link

- item with a fence

  ```py
  in_a_list_item()
  ```

> A quote with ![img-in-quote](quoted.png), [link-in-quote](https://example.com/q),
> `code in quote`, **strong in quote**, and a
> [![badge-in-quote](qbadge.png)](https://example.com/qb) badge.

> [!NOTE] A callout
> - bullet in a callout
>
> ```rust
> fence_after_a_list_in_a_callout();
> ```

> - quoted tight one
> - quoted tight two

> 1. quoted ordered one
> 2. quoted ordered two

> - [x] quoted done
> - [ ] quoted todo

> - quoted outer
>   - quoted nested

> quoted paragraph
>
> ```sh
> fence_after_a_paragraph_in_a_quote
> ```

> ```sh
> fence_first_in_a_quote
> ```
>
> - list after the fence

> - quoted step
>
>   a continuation paragraph of the item above

> - quoted step with a fence
>
>   ```sh
>   fence_inside_a_quoted_item
>   ```

> outer quote
>
> > nested quote

| Column A | Column B | Column C |
|:---------|:--------:|---------:|
| left     | center   | right    |
| a        | b        | c        |

<details>
<summary>A details block</summary>

Hidden body text with a ![img-in-details](details.png) image.

</details>

```
fence with no language, after the details block
```

Final paragraph, so the last block is not a special case.
