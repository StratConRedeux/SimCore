// Bridge benchmark. Run in release: `swift run -c release simcore-bench [units] [iterations]`.
import SimCoreBench

let args = CommandLine.arguments.dropFirst().compactMap { Int($0) }
let units = args.count > 0 ? UInt32(args[0]) : 50_000
let iterations = args.count > 1 ? args[1] : 300
print(BridgeBenchmark.run(units: units, iterations: iterations).formatted())
