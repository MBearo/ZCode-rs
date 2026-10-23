// 构建产物的静态检查（docs/specs/rust-cli/linux.md §4）：读 ELF 的头、程序头与 `.dynamic`，找本机路径。纯函数。

const PT_LOAD = 1;
const PT_DYNAMIC = 2;
const PT_INTERP = 3;
const DT_NEEDED = 1n;
const DT_STRTAB = 5n;
const TYPES = { 1: "REL", 2: "EXEC", 3: "DYN", 4: "CORE" };

/** 64 位小端 ELF 的格式、解释器（`PT_INTERP`）与需要的共享库（`DT_NEEDED`）。别的格式抛错。 */
export function inspectElf(buf) {
  if (buf.length < 64 || buf.toString("latin1", 0, 4) !== "\x7fELF")
    throw new Error("不是 ELF 文件");
  if (buf[4] !== 2) throw new Error(`不是 64 位 ELF（EI_CLASS=${buf[4]}）`);
  if (buf[5] !== 1) throw new Error(`不是 little-endian 的 ELF（EI_DATA=${buf[5]}）`);
  const u64 = (at) => Number(buf.readBigUInt64LE(at));
  const phoff = u64(32);
  const phentsize = buf.readUInt16LE(54);
  const phnum = buf.readUInt16LE(56);
  const phdrs = [];
  for (let i = 0; i < phnum; i += 1) {
    const at = phoff + i * phentsize;
    phdrs.push({
      type: buf.readUInt32LE(at),
      offset: u64(at + 8),
      vaddr: u64(at + 16),
      filesz: u64(at + 32),
    });
  }
  const cstring = (at) => {
    const end = buf.indexOf(0, at);
    return buf.toString("latin1", at, end < 0 ? buf.length : end);
  };
  const interp = phdrs.find((ph) => ph.type === PT_INTERP);
  const dynamic = phdrs.find((ph) => ph.type === PT_DYNAMIC);
  const needed = [];
  if (dynamic) {
    const names = [];
    let strtab;
    for (let at = dynamic.offset; at + 16 <= dynamic.offset + dynamic.filesz; at += 16) {
      const tag = buf.readBigInt64LE(at);
      if (tag === 0n) break;
      if (tag === DT_NEEDED) names.push(u64(at + 8));
      if (tag === DT_STRTAB) strtab = u64(at + 8);
    }
    // DT_STRTAB 是虚拟地址：经包含它的 PT_LOAD 换成文件偏移。
    const load = phdrs.find(
      (ph) =>
        ph.type === PT_LOAD &&
        strtab !== undefined &&
        strtab >= ph.vaddr &&
        strtab < ph.vaddr + ph.filesz,
    );
    for (const name of names) {
      needed.push(
        load ? cstring(load.offset + (strtab - load.vaddr) + name) : `<字符串表偏移 ${name}>`,
      );
    }
  }
  return {
    bits: 64,
    endian: "little",
    type: TYPES[buf.readUInt16LE(16)] ?? String(buf.readUInt16LE(16)),
    machine: buf.readUInt16LE(18),
    interpreter: interp ? cstring(interp.offset) : null,
    needed,
  };
}

const printable = (byte) => byte >= 0x20 && byte < 0x7f;

/**
 * 每个出现过的本机路径前缀：次数与第一处周围的可打印文字（去重，按给出的顺序）。`allowed` 是程序自己的字面量
 *（例如找 shell 的候选目录 `/opt/homebrew/bin`，linux.md §4、§8.1）：从那一处开始正是它时不算。
 */
export function hostPathHits(buf, needles, allowed = []) {
  const hits = [];
  const literals = allowed.map((literal) => Buffer.from(literal));
  const isLiteral = (at) =>
    literals.some((literal) => buf.subarray(at, at + literal.length).equals(literal));
  for (const needle of new Set(needles)) {
    const bytes = Buffer.from(needle);
    let count = 0;
    let first = -1;
    for (let at = buf.indexOf(bytes); at >= 0; at = buf.indexOf(bytes, at + bytes.length)) {
      if (isLiteral(at)) continue;
      if (first < 0) first = at;
      count += 1;
    }
    if (count === 0) continue;
    let start = first;
    while (start > 0 && first - start < 80 && printable(buf[start - 1])) start -= 1;
    let end = first;
    while (end < buf.length && end - first < 160 && printable(buf[end])) end += 1;
    hits.push({ needle, count, sample: buf.toString("latin1", start, end) });
  }
  return hits;
}

/** mimalloc 编进来的字符串（它的消息前缀，`options.c` 的 `"mimalloc: "`），linux.md §8.3。 */
const ALLOCATOR_MARKER = Buffer.from("mimalloc: ");

/** 产物里有 mimalloc（musl 版的全局分配器）。 */
export function hasAllocator(buf) {
  return buf.indexOf(ALLOCATOR_MARKER) >= 0;
}

/** 不通过的项（空数组表示通过）：架构、动态加载器、共享库、本机路径、分配器。 */
export function checkArtifact({ elf, hits, machine, allocator }) {
  const problems = [];
  if (elf.machine !== machine) problems.push(`架构不对：e_machine=${elf.machine}，应为 ${machine}`);
  if (elf.interpreter !== null) problems.push(`需要动态加载器 ${elf.interpreter}（不是静态链接）`);
  if (elf.needed.length) problems.push(`需要共享库 ${elf.needed.join("、")}（不是静态链接）`);
  for (const hit of hits)
    problems.push(`本机路径 ${hit.needle} 出现 ${hit.count} 次，例如：${hit.sample}`);
  if (!allocator) problems.push("没有 mimalloc（linux.md §8.3：musl 版的全局分配器）");
  return problems;
}
