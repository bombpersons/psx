// PS-X EXE setup, index export and bounded function decompilation for psx.
// The Rust supervisor passes a JSON request file; stdout is reserved for Ghidra logs.
// @category PSX
import com.google.gson.*;
import ghidra.app.util.headless.HeadlessScript;
import ghidra.app.cmd.disassemble.DisassembleCommand;
import ghidra.program.disassemble.Disassembler;
import ghidra.app.cmd.function.ApplyFunctionSignatureCmd;
import ghidra.app.decompiler.*;
import ghidra.app.decompiler.component.DecompilerUtils;
import ghidra.program.model.data.*;
import ghidra.program.model.pcode.*;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.math.BigInteger;
import java.util.*;

public class PsxGhidra extends HeadlessScript {
    private final Gson gson = new GsonBuilder().setPrettyPrinting().create();
    private JsonObject request;
    private final JsonArray warnings = new JsonArray();

    private String hex(Address a) { return String.format("0x%08X", a.getOffset()); }
    private Address address(String key) { return toAddr(request.get(key).getAsLong()); }
    private void write(String key, JsonObject value) throws Exception {
        try (var out = Files.newBufferedWriter(Path.of(request.get(key).getAsString()), StandardCharsets.UTF_8)) {
            gson.toJson(value, out);
        }
    }
    private JsonObject function(Function f) {
        var out = new JsonObject();
        out.addProperty("entry", hex(f.getEntryPoint()));
        out.addProperty("name", f.getName());
        out.addProperty("name_source", f.getSymbol().getSource().toString());
        out.addProperty("signature", f.getSignature().toString());
        out.addProperty("comment", f.getComment());
        var body = new JsonArray();
        for (var range : f.getBody()) {
            var r = new JsonObject();
            r.addProperty("start", hex(range.getMinAddress()));
            r.addProperty("end_inclusive", hex(range.getMaxAddress()));
            body.add(r);
        }
        out.add("body", body);
        return out;
    }
    private void setup() throws Exception {
        var memory = currentProgram.getMemory();
        var block = memory.getBlock(address("base"));
        block.setRead(true);
        block.setWrite(true);
        block.setExecute(false);
        var code = new AddressSet();
        for (var item : request.getAsJsonArray("code_ranges")) {
            var r = item.getAsJsonArray();
            var lo = toAddr(r.get(0).getAsLong());
            var hi = toAddr(r.get(1).getAsLong() - 1);
            code.add(lo, hi);
            var part = memory.getBlock(lo);
            if (!part.getStart().equals(lo)) { memory.split(part, lo); part = memory.getBlock(lo); }
            if (part.getEnd().compareTo(hi) > 0) memory.split(part, hi.add(1));
            part.setExecute(true);
        }
        currentProgram.getOptions(Program.DISASSEMBLER_PROPERTIES).setBoolean(
            Disassembler.RESTRICT_DISASSEMBLY_TO_EXECUTE_MEMORY_PROPERTY, true);
        long bssSize = request.get("bss_size").getAsLong();
        if (bssSize != 0) {
            var bss = memory.createUninitializedBlock("bss", address("bss_addr"), bssSize, false);
            bss.setRead(true);
            bss.setWrite(true);
            bss.setExecute(false);
        }
        long gp = request.get("gp").getAsLong();
        if (gp != 0) {
            currentProgram.getProgramContext().setValue(currentProgram.getRegister("gp"),
                toAddr(request.get("base").getAsLong()), toAddr(request.get("base").getAsLong() + request.get("size").getAsLong() - 1), BigInteger.valueOf(gp));
        }
        for (var item : request.getAsJsonArray("entries")) {
            var entry = toAddr(item.getAsLong());
            currentProgram.getSymbolTable().addExternalEntryPoint(entry);
            new DisassembleCommand(entry, code, true).applyTo(currentProgram, monitor);
            if (getFunctionAt(entry) == null) createFunction(entry, null);
        }
        for (var item : request.getAsJsonArray("symbols")) {
            monitor.checkCancelled();
            var s = item.getAsJsonObject();
            var at = toAddr(s.get("address").getAsLong());
            try {
                if (s.get("kind").getAsString().equals("function")) {
                    new DisassembleCommand(at, code, true).applyTo(currentProgram, monitor);
                    var f = getFunctionAt(at);
                    if (f == null) f = createFunction(at, null);
                    if (f == null) throw new IOException("function overlaps existing code");
                    f.setName(s.get("name").getAsString(), SourceType.USER_DEFINED);
                } else {
                    createLabel(at, s.get("name").getAsString(), false, SourceType.USER_DEFINED);
                }
            } catch (Exception e) {
                warnings.add("symbol " + hex(at) + ": " + e.getMessage());
            }
        }
        var result = new JsonObject();
        result.add("warnings", warnings);
        write("setup_result", result);
    }
    private void exportIndex() throws Exception {
        var index = new JsonObject();
        index.addProperty("analysis_timed_out", analysisTimeoutOccurred());
        var functions = new JsonArray();
        for (var f : currentProgram.getFunctionManager().getFunctions(true)) {
            monitor.checkCancelled();
            functions.add(function(f));
        }
        index.add("functions", functions);
        var refs = new JsonArray();
        int cop2 = 0, rfe = 0;
        var listing = currentProgram.getListing();
        for (var it = listing.getInstructions(true); it.hasNext();) {
            monitor.checkCancelled();
            var instruction = it.next();
            int word = currentProgram.getMemory().getInt(instruction.getAddress());
            if ((word >>> 26) == 0x12 || (word >>> 26) == 0x32 || (word >>> 26) == 0x3a) cop2++;
            if (word == 0x42000010) rfe++;
            for (var ref : instruction.getReferencesFrom()) {
                if (refs.size() >= 500000) break;
                var row = new JsonObject();
                row.addProperty("from", hex(ref.getFromAddress()));
                row.addProperty("to", hex(ref.getToAddress()));
                row.addProperty("to_space", ref.getToAddress().getAddressSpace().getName());
                row.addProperty("type", ref.getReferenceType().toString());
                row.addProperty("call", ref.getReferenceType().isCall());
                var owner = getFunctionContaining(ref.getFromAddress());
                if (owner != null) row.addProperty("function", hex(owner.getEntryPoint()));
                refs.add(row);
            }
        }
        index.add("references", refs);
        index.addProperty("references_truncated", refs.size() >= 500000);
        index.addProperty("cop2_instructions", cop2);
        index.addProperty("rfe_instructions", rfe);
        write("index", index);
    }
    // Names (psx_symbols::names, as JSON): applied for one query in the
    // read-only session, so nothing stays in the project.
    private JsonObject names;
    private final JsonArray nameErrors = new JsonArray();
    private final Map<String, DataType> types = new HashMap<>();
    private final Map<String, JsonObject> typeDefs = new HashMap<>();
    private final JsonObject applied = new JsonObject();
    private final CategoryPath category = new CategoryPath("/psx");

    private String at(JsonObject item) {
        var a = item.getAsJsonObject("at");
        return a.get("file").getAsString() + ":" + a.get("line").getAsInt();
    }
    private void nameError(JsonObject item, String message) { nameErrors.add(at(item) + ": " + message); }
    private String name(JsonObject item) {
        var n = item.getAsJsonObject("name");
        return n.get("text").getAsString() + (n.has("guess") && n.get("guess").getAsBoolean() ? "?" : "");
    }
    private void count(String kind) {
        applied.addProperty(kind, (applied.has(kind) ? applied.get(kind).getAsInt() : 0) + 1);
    }
    private static final Map<String, DataType> BUILTIN = new HashMap<>();
    private static final Map<String, String> ALIAS = new HashMap<>();
    static {
        BUILTIN.put("void", VoidDataType.dataType); BUILTIN.put("char", CharDataType.dataType);
        BUILTIN.put("uchar", UnsignedCharDataType.dataType); BUILTIN.put("byte", ByteDataType.dataType);
        BUILTIN.put("bool", BooleanDataType.dataType); BUILTIN.put("short", ShortDataType.dataType);
        BUILTIN.put("ushort", UnsignedShortDataType.dataType); BUILTIN.put("int", IntegerDataType.dataType);
        BUILTIN.put("uint", UnsignedIntegerDataType.dataType); BUILTIN.put("long", IntegerDataType.dataType);
        BUILTIN.put("ulong", UnsignedIntegerDataType.dataType); BUILTIN.put("float", FloatDataType.dataType);
        BUILTIN.put("double", DoubleDataType.dataType); BUILTIN.put("longlong", LongLongDataType.dataType);
        BUILTIN.put("undefined", DataType.DEFAULT); BUILTIN.put("undefined1", Undefined1DataType.dataType);
        BUILTIN.put("undefined2", Undefined2DataType.dataType); BUILTIN.put("undefined4", Undefined4DataType.dataType);
        BUILTIN.put("undefined8", Undefined8DataType.dataType);
        // Spellings Ghidra lacks become typedefs, so the decompile keeps them.
        for (var a : new String[][] {{"i8", "schar"}, {"s8", "schar"}, {"u8", "uchar"}, {"u_char", "uchar"},
                {"i16", "short"}, {"s16", "short"}, {"u16", "ushort"}, {"u_short", "ushort"},
                {"i32", "int"}, {"s32", "int"}, {"u32", "uint"}, {"u_int", "uint"}, {"u_long", "uint"},
                {"i64", "longlong"}, {"s64", "longlong"}, {"u64", "ulonglong"}}) ALIAS.put(a[0], a[1]);
    }
    private DataType builtin(String n) {
        if (n.equals("schar")) return SignedCharDataType.dataType;
        if (n.equals("ulonglong")) return UnsignedLongLongDataType.dataType;
        if (n.equals("code")) return new FunctionDefinitionDataType(category, "code");
        return BUILTIN.get(n);
    }
    private DataType add(DataType t) {
        return currentProgram.getDataTypeManager().addDataType(t, DataTypeConflictHandler.REPLACE_HANDLER);
    }
    private DataType named(String n, List<String> seen) throws Exception {
        if (types.containsKey(n)) return types.get(n);
        var b = builtin(n);
        if (b != null) return b;
        if (ALIAS.containsKey(n)) {
            var t = add(new TypedefDataType(category, n, builtin(ALIAS.get(n))));
            types.put(n, t);
            return t;
        }
        var def = typeDefs.get(n);
        if (def == null) throw new IOException("unknown type " + n);
        if (seen.contains(n)) throw new IOException("type " + n + " holds itself");
        seen.add(n);
        var body = def.getAsJsonObject("body");
        // Structs, unions and enums are made before their uses; a typedef here.
        var t = add(new TypedefDataType(category, n, type(body.getAsJsonObject("type"), seen)));
        types.put(n, t);
        return t;
    }
    private DataType type(JsonObject t, List<String> seen) throws Exception {
        switch (t.get("kind").getAsString()) {
            case "named": return named(t.get("name").getAsString(), seen);
            case "pointer": {
                // A pointer's target may be incomplete, so the cycle check stops here.
                var to = type(t.getAsJsonObject("to"), new ArrayList<>());
                long offset = t.has("offset") ? t.get("offset").getAsLong() : 0;
                if (offset == 0) return new PointerDataType(to, 4, currentProgram.getDataTypeManager());
                // Named as the names file writes it, which the decompile prints.
                // Ghidra names an offset pointer itself (Object *32 __((offset(0x38))))
                // and prints names as identifiers; a typedef reads better.
                var pointer = add(new PointerTypedefBuilder(to, 4, currentProgram.getDataTypeManager())
                    .type(PointerType.DEFAULT).componentOffset(offset).build());
                return add(new TypedefDataType(category, to.getName() + "_ptr_0x" + Long.toHexString(offset), pointer));
            }
            case "array": {
                var of = type(t.getAsJsonObject("of"), seen);
                if (of.getLength() <= 0) throw new IOException("an array of " + of.getName() + ", which has no size");
                return new ArrayDataType(of, t.get("len").getAsInt(), of.getLength());
            }
            case "function": return signature("", t);
            default: throw new IOException("unknown type kind " + t.get("kind"));
        }
    }
    private FunctionDefinitionDataType signature(String name, JsonObject s) throws Exception {
        var f = new FunctionDefinitionDataType(category, name.isEmpty() ? "fn" : name);
        f.setReturnType(type(s.getAsJsonObject("ret"), new ArrayList<>()));
        var params = new ArrayList<ParameterDefinition>();
        int i = 1;
        for (var item : s.getAsJsonArray("params")) {
            var p = item.getAsJsonObject();
            var t = type(p.getAsJsonObject("type"), new ArrayList<>());
            if (t instanceof VoidDataType) continue;
            params.add(new ParameterDefinitionImpl(p.has("name") ? name(p) : "param_" + i, t, null));
            i++;
        }
        f.setArguments(params.toArray(new ParameterDefinition[0]));
        f.setVarArgs(s.has("varargs") && s.get("varargs").getAsBoolean());
        f.setCallingConvention(currentProgram.getCompilerSpec().getDefaultCallingConvention().getName());
        return f;
    }
    private void applyTypes() {
        var made = new ArrayList<JsonObject>();
        for (var item : names.getAsJsonArray("types")) {
            var d = item.getAsJsonObject();
            var n = d.getAsJsonObject("name").get("text").getAsString();
            typeDefs.put(n, d);
            var body = d.getAsJsonObject("body");
            var kind = body.get("kind").getAsString();
            try {
                int size = d.has("size") ? d.get("size").getAsInt() : 4;
                DataType t = null;
                if (kind.equals("struct")) t = new StructureDataType(category, n, size);
                else if (kind.equals("union")) t = new UnionDataType(category, n);
                else if (kind.equals("enum")) {
                    var e = new EnumDataType(category, n, size <= 0 ? 4 : size);
                    for (var v : body.getAsJsonArray("values")) {
                        var o = v.getAsJsonObject();
                        e.add(o.getAsJsonObject("name").get("text").getAsString(), o.get("value").getAsLong());
                    }
                    t = e;
                }
                if (t != null) { types.put(n, add(t)); made.add(d); }
            } catch (Exception e) { nameError(d, e.getMessage()); }
        }
        for (var d : made) {
            var n = d.getAsJsonObject("name").get("text").getAsString();
            var body = d.getAsJsonObject("body");
            if (!body.has("fields")) { count("types"); continue; }
            var t = types.get(n);
            for (var item : body.getAsJsonArray("fields")) {
                var f = item.getAsJsonObject();
                try {
                    var ft = type(f.getAsJsonObject("type"), new ArrayList<>(List.of(n)));
                    var comment = f.has("comment") ? f.get("comment").getAsString() : null;
                    if (t instanceof Structure st) st.replaceAtOffset(f.get("offset").getAsInt(), ft, ft.getLength(), name(f), comment);
                    else ((Union) t).add(ft, ft.getLength(), name(f), comment);
                } catch (Exception e) { nameError(f, name(f) + ": " + e.getMessage()); }
            }
            count("types");
        }
        for (var d : typeDefs.values()) {
            if (!d.getAsJsonObject("body").get("kind").getAsString().equals("typedef")) continue;
            try { named(d.getAsJsonObject("name").get("text").getAsString(), new ArrayList<>()); count("types"); }
            catch (Exception e) { nameError(d, e.getMessage()); }
        }
    }
    private void applyNames() {
        int transaction = currentProgram.startTransaction("psx names");
        try {
            applyTypes();
            var memory = currentProgram.getMemory();
            for (var item : names.getAsJsonArray("globals")) {
                var g = item.getAsJsonObject();
                var a = toAddr(g.get("address").getAsString());
                try {
                    createLabel(a, name(g), true, SourceType.USER_DEFINED);
                    if (g.has("comment")) setPlateComment(a, g.get("comment").getAsString());
                    if (g.has("type")) {
                        var t = type(g.getAsJsonObject("type"), new ArrayList<>());
                        var end = a.add(Math.max(t.getLength(), 1) - 1);
                        // Past the image (BSS the header leaves out): memory for this query alone.
                        if (!memory.intersects(a, end))
                            memory.createUninitializedBlock("names_" + hex(a).substring(2), a, end.subtract(a) + 1, false);
                        if (!memory.contains(a, end))
                            throw new IOException("its type runs past the memory this analysis holds (named only)");
                        clearListing(a, end);
                        createData(a, t);
                    }
                    count("globals");
                } catch (Exception e) { nameError(g, name(g) + ": " + e.getMessage()); }
            }
            var code = new AddressSet();
            for (var block : memory.getBlocks()) if (block.isExecute()) code.add(block.getStart(), block.getEnd());
            for (var item : names.getAsJsonArray("functions")) {
                var fn = item.getAsJsonObject();
                var a = toAddr(fn.get("address").getAsString());
                if (!memory.contains(a)) continue;
                try {
                    var f = getFunctionAt(a);
                    if (f == null) {
                        new DisassembleCommand(a, code, true).applyTo(currentProgram, monitor);
                        f = createFunction(a, null);
                    }
                    if (f == null) throw new IOException("no function can start here (inside another, or not code)");
                    if (fn.has("signature")) {
                        var sig = signature(name(fn), fn.getAsJsonObject("signature"));
                        if (!new ApplyFunctionSignatureCmd(a, sig, SourceType.USER_DEFINED).applyTo(currentProgram, monitor))
                            throw new IOException("signature not applied");
                    }
                    f.setName(name(fn), SourceType.USER_DEFINED);
                    if (fn.has("comment")) f.setComment(fn.get("comment").getAsString());
                    count("functions");
                } catch (Exception e) { nameError(fn, name(fn) + ": " + e.getMessage()); }
            }
        } finally { currentProgram.endTransaction(transaction, true); }
    }
    // The stack offset at entry of a local given after the prologue (sp+N).
    private int frameSize(Function f) throws Exception {
        var a = f.getEntryPoint();
        for (int i = 0; i < 16; i++) {
            int w = currentProgram.getMemory().getInt(a.add(i * 4));
            if ((w >>> 16) == 0x27BD && (short) w < 0) return -(short) w;
        }
        return 0;
    }
    private HighFunction high(DecompInterface d, Function f) {
        var r = d.decompileFunction(f, request.get("decompile_timeout").getAsInt(), monitor);
        return r.decompileCompleted() ? r.getHighFunction() : null;
    }
    private void applyLocals(DecompInterface d, Function f) {
        JsonObject mine = null;
        for (var item : names.getAsJsonArray("functions")) {
            var fn = item.getAsJsonObject();
            if (toAddr(fn.get("address").getAsString()).equals(f.getEntryPoint()) && fn.has("locals")) mine = fn;
        }
        if (mine == null) return;
        int transaction = currentProgram.startTransaction("psx locals");
        try {
            var h = high(d, f);
            if (h == null) return;
            // Locals are kept against the parameters the decompiler found.
            if (f.getSignatureSource() == SourceType.DEFAULT) {
                HighFunctionDBUtil.commitParamsToDatabase(h, true, HighFunctionDBUtil.ReturnCommitOption.NO_COMMIT, SourceType.ANALYSIS);
                h = high(d, f);
                if (h == null) return;
            }
            var symbols = new ArrayList<HighSymbol>();
            h.getLocalSymbolMap().getSymbols().forEachRemaining(symbols::add);
            int frame = frameSize(f);
            for (var item : mine.getAsJsonArray("locals")) {
                var l = item.getAsJsonObject();
                var slot = l.getAsJsonObject("slot");
                String kind = slot.get("kind").getAsString();
                HighSymbol found = null;
                for (var s : symbols) {
                    var st = s.getStorage();
                    if (st == null || st.isBadStorage()) continue;
                    if (kind.equals("register")) {
                        var r = slot.get("at").getAsString().replace("fp", "s8");
                        if (st.isRegisterStorage() && st.getRegister().getName().equalsIgnoreCase(r)) { found = s; break; }
                    } else if (st.isStackStorage()) {
                        int off = slot.get("at").getAsInt() - (kind.equals("frame") ? frame : 0);
                        if (st.getStackOffset() == off) { found = s; break; }
                    }
                }
                try {
                    if (found == null) throw new IOException("the decompile has no local in that slot");
                    var t = l.has("type") ? type(l.getAsJsonObject("type"), new ArrayList<>()) : null;
                    HighFunctionDBUtil.updateDBVariable(found, name(l), t, SourceType.USER_DEFINED);
                    count("locals");
                } catch (Exception e) { nameError(l, name(l) + ": " + e.getMessage()); }
            }
        } catch (Exception e) {
            nameErrors.add("locals of " + f.getName() + ": " + e.getMessage());
        } finally { currentProgram.endTransaction(transaction, true); }
    }
    private void decompileInto(DecompInterface decompiler, Function f, JsonObject result) throws Exception {
        if (names != null) applyLocals(decompiler, f);
        var output = decompiler.decompileFunction(f, request.get("decompile_timeout").getAsInt(), monitor);
        result.addProperty("status", output.decompileCompleted() ? "complete" : output.isTimedOut() ? "timeout" : "decompile_failed");
        result.addProperty("diagnostic", output.getErrorMessage());
        if (!output.decompileCompleted()) return;
        result.addProperty("pseudocode", output.getDecompiledFunction().getC());
        result.getAsJsonObject("function").addProperty("signature", output.getDecompiledFunction().getSignature());
        // Each line's instruction addresses, and what is still unnamed: callees
        // and globals by Ghidra's default names, struct offsets in no field.
        var lines = new JsonArray();
        var unnamed = new TreeMap<String, Integer>();
        for (var line : DecompilerUtils.toLines(output.getCCodeMarkup())) {
            var at = new TreeSet<Long>();
            for (var t : line.getAllTokens()) {
                if (t.getMinAddress() != null) at.add(t.getMinAddress().getOffset());
                String text = t.getText(), key = null;
                if (t instanceof ClangFuncNameToken && text.startsWith("FUN_") && !text.equals(f.getName())) key = text;
                else if (t instanceof ClangVariableToken && text.matches("_*(DAT|PTR)_[0-9A-Za-z_]+")) key = text.replaceFirst("^_+", "");
                else if (t instanceof ClangFieldToken ft && text.startsWith("field_0x") && ft.getDataType() != null)
                    key = ft.getDataType().getName() + "+" + text.substring("field_".length());
                if (key != null) unnamed.merge(key, 1, Integer::sum);
            }
            if (at.isEmpty()) continue;
            var l = new JsonArray();
            l.add(line.getLineNumber());
            for (long a : at) l.add(a);
            lines.add(l);
        }
        result.add("lines", lines);
        var u = new JsonObject();
        unnamed.forEach(u::addProperty);
        result.add("unnamed", u);
    }
    private void decompile() throws Exception {
        var result = new JsonObject();
        var at = request.has("addr") ? address("addr") : null;
        if (request.has("names") && request.get("names").isJsonObject()) {
            names = request.getAsJsonObject("names");
            applyNames();
        }
        var decompiler = new DecompInterface();
        try {
            if (!decompiler.openProgram(currentProgram)) throw new IOException(decompiler.getLastMessage());
            if (request.has("addrs")) {
                // Several functions in one launch; an address in none is left out.
                var all = new JsonArray();
                var done = new HashSet<Address>();
                for (var item : request.getAsJsonArray("addrs")) {
                    var f = getFunctionContaining(toAddr(item.getAsLong()));
                    if (f == null || !done.add(f.getEntryPoint())) continue;
                    var one = new JsonObject();
                    one.add("function", function(f));
                    decompileInto(decompiler, f, one);
                    all.add(one);
                }
                result.addProperty("status", "complete");
                result.add("functions", all);
            } else {
                var f = getFunctionContaining(at);
                if (f == null) {
                    result.addProperty("status", "function_not_found");
                    result.addProperty("diagnostic", "No analysed function contains this address; use a known entry or supply symbols to seed analysis.");
                    write("query_result", result);
                    return;
                }
                result.add("function", function(f));
                decompileInto(decompiler, f, result);
            }
        } finally {
            decompiler.dispose();
        }
        if (names != null) {
            var n = new JsonObject();
            n.add("applied", applied);
            n.add("errors", nameErrors);
            result.add("names", n);
        }
        write("query_result", result);
    }
    @Override public void run() throws Exception {
        try (var in = Files.newBufferedReader(Path.of(getScriptArgs()[0]), StandardCharsets.UTF_8)) {
            request = JsonParser.parseReader(in).getAsJsonObject();
        }
        String mode = request.get("mode").getAsString();
        if (mode.equals("setup")) { setup(); return; }
        if (mode.equals("export")) exportIndex();
        if (request.has("addr") || request.has("addrs")) decompile();
    }
}
