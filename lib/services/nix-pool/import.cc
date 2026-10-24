// Trusted startup import using native Nix parsing/registration with disk-backed NAR buffering.
#include "nix/main/shared.hh"
#include "nix/store/store-api.hh"
#include "nix/store/store-open.hh"
#include "nix/store/export-import.hh"
#include "nix/store/common-protocol.hh"
#include "nix/store/common-protocol-impl.hh"
#include "nix/util/archive.hh"
#include "nix/util/file-system.hh"
#include "nix/util/hash.hh"
#include "nix/util/serialise.hh"
#include <filesystem>
#include <fstream>
#include <iostream>
#include <unistd.h>

int main(int argc, char ** argv)
{
    try {
        std::ifstream marker("/etc/wrix/nix-pool-service");
        std::string role((std::istreambuf_iterator<char>(marker)), {});
        if (geteuid() != 0 || role != "1\n" || std::filesystem::exists("/etc/wrix/image-agent") || argc < 2)
            throw nix::Error("trusted image import requires its fixed service role and expected paths");
        nix::initNix();
        auto store = nix::openStore("daemon");
        nix::StorePathSet expected;
        for (int i = 1; i < argc; ++i)
            if (!expected.insert(store->parseStorePath(argv[i])).second)
                throw nix::Error("duplicate expected image input");
        nix::FdSource input(STDIN_FILENO);
        while (true) {
            auto tag = nix::readNum<uint64_t>(input);
            if (tag == 0) break;
            if (tag != 1) throw nix::Error("invalid native image archive");
            auto saved = nix::createAnonymousTempFile();
            nix::FdSink disk(saved.get());
            nix::HashSink hash(nix::HashAlgorithm::SHA256);
            nix::TeeSink sinks(disk, hash);
            nix::TeeSource source(input, sinks);
            nix::NullFileSystemObjectSink discard;
            nix::parseDump(discard, source);
            disk.flush();
            auto result = hash.finish();
            if (nix::readInt(input) != nix::exportMagic)
                throw nix::Error("invalid native image archive metadata");
            auto path = store->parseStorePath(nix::readString(input));
            if (expected.erase(path) != 1)
                throw nix::Error("unexpected or repeated native image input");
            auto references = nix::CommonProto::Serialise<nix::StorePathSet>::read(*store, {.from = input});
            auto deriver = nix::readString(input);
            nix::ValidPathInfo info{path, {*store, result.hash}};
            if (!deriver.empty()) info.deriver = store->parseStorePath(deriver);
            info.references = references;
            info.narSize = result.numBytesDigested;
            if (nix::readInt(input) == 1) nix::readString(input);
            nix::FdSource nar(saved.get());
            nar.restart();
            // Match trusted nix-store --import; only the fenced startup role reaches this call.
            store->addToStore(info, nar, nix::NoRepair, nix::NoCheckSigs);
            std::cout << store->printStorePath(path) << '\n';
        }
        if (!expected.empty()) throw nix::Error("native image archive omitted required inputs");
        std::cout.flush();
        if (!std::cout) throw nix::Error("native image receipt could not be written");
        return 0;
    } catch (const std::exception & error) {
        std::cerr << "image import failed: " << error.what() << '\n';
        return 1;
    }
}
