// PS-X EXE setup, index export and bounded function decompilation for psx.
// The Rust supervisor passes a JSON request file; stdout is reserved for Ghidra logs.
// @category PSX
import com.google.gson.*;
import ghidra.app.util.headless.HeadlessScript;
import ghidra.app.decompiler.DecompInterface;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.math.BigInteger;

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
        block.setExecute(true);
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
                block.getStart(), block.getEnd(), BigInteger.valueOf(gp));
        }
        var entry = address("entry");
        currentProgram.getSymbolTable().addExternalEntryPoint(entry);
        disassemble(entry);
        if (getFunctionAt(entry) == null) createFunction(entry, "psx_entry");
        for (var item : request.getAsJsonArray("symbols")) {
            monitor.checkCancelled();
            var s = item.getAsJsonObject();
            var at = toAddr(s.get("address").getAsLong());
            try {
                if (s.get("kind").getAsString().equals("function")) {
                    disassemble(at);
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
    private void decompile() throws Exception {
        var result = new JsonObject();
        var at = address("addr");
        var f = getFunctionContaining(at);
        if (f == null) {
            result.addProperty("status", "function_not_found");
            result.addProperty("diagnostic", "No analysed function contains this address; use a known entry or supply symbols to seed analysis.");
            write("query_result", result);
            return;
        }
        result.add("function", function(f));
        var decompiler = new DecompInterface();
        try {
            if (!decompiler.openProgram(currentProgram)) throw new IOException(decompiler.getLastMessage());
            var output = decompiler.decompileFunction(f, request.get("decompile_timeout").getAsInt(), monitor);
            result.addProperty("status", output.decompileCompleted() ? "complete" : output.isTimedOut() ? "timeout" : "decompile_failed");
            result.addProperty("diagnostic", output.getErrorMessage());
            if (output.decompileCompleted()) {
                result.addProperty("pseudocode", output.getDecompiledFunction().getC());
                result.getAsJsonObject("function").addProperty("signature", output.getDecompiledFunction().getSignature());
            }
        } finally {
            decompiler.dispose();
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
        if (request.has("addr")) decompile();
    }
}
