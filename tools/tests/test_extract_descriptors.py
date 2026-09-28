import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]
GENERATOR = REPO / "tools" / "extract_descriptors.py"

ENTRY_RE = re.compile(
    r'OverlayEntry \{ group_path: "([^"]+)", field_name: "([^"]+)", '
    r'field_type: ([^}]+(?:\{[^}]+\})?) \},'
)
HANDLE_ENTRY_RE = re.compile(
    r'OverlayHandleEntry \{ group_path: "([^"]+)", handle: (\d+), '
    r'field_name: "([^"]+)" \},'
)
RUNTIME_AGENT_CACHE_FACTORY = r'''
internal static class AgentClassNetCacheDescriptors
{
    private const string KillFunctionName = "MulticastNotifyKilledEnemy";
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors)
    {
        return agentDescriptors
            .Select(agent => new ClassNetCacheDescriptor(
                agent.Path + "_ClassNetCache", [CreateKillRpc()]))
            .ToArray();
    }
    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = KillFunctionName,
    };
}
'''


class GeneratorHarness:
    """The generator-running helpers both test classes below share.

    A plain mixin, not a TestCase. `SilentDropTests` used to get these by
    subclassing `ExtractDescriptorsTests`, and unittest collects inherited
    `test_*` methods, so every test in that class -- 67 of them, each spawning
    a generator subprocess -- ran a second time under `SilentDropTests`' name
    and inflated the suite size the docs quote. The `assert*` calls resolve
    through the `unittest.TestCase` each concrete class also inherits.
    """

    def run_generator_process(
        self, sources: dict[str, str]
    ) -> tuple[subprocess.CompletedProcess[str], str | None]:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source_root = root / "Replay.Valorant"
            source_root.mkdir()
            for relative_path, source in sources.items():
                path = source_root / relative_path
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source, encoding="utf-8")

            output = root / "table.rs"
            result = subprocess.run(
                [sys.executable, str(GENERATOR), str(source_root), str(output)],
                check=False,
                capture_output=True,
                text=True,
                encoding="utf-8",
            )
            output_text = output.read_text(encoding="utf-8") if output.exists() else None
            return result, output_text

    def run_generator(self, sources: dict[str, str]) -> str:
        result, output = self.run_generator_process(sources)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIsNotNone(output)
        return output or ""

    def run_generator_expecting_failure(self, sources: dict[str, str]) -> str:
        result, _ = self.run_generator_process(sources)
        self.assertNotEqual(result.returncode, 0, "generator unexpectedly succeeded")
        return result.stderr

    def runtime_cache_entries(self, output: str) -> set[tuple[str, str, str]]:
        return {
            (group, field, field_type.strip())
            for group, field, field_type in ENTRY_RE.findall(output)
            if group.endswith("_ClassNetCache")
        }

    def entries(self, output: str) -> set[tuple[str, str]]:
        return {
            (group, field) for group, field, _ in ENTRY_RE.findall(output)
        }


class ExtractDescriptorsTests(GeneratorHarness, unittest.TestCase):
    def test_virtual_movement_uses_concrete_override(self):
        output = self.run_generator({"Movement.cs": r'''
public abstract class BaseFlash<T> : ExportGroupDescriptor<T>
{
    protected virtual ERotatorQuantization MovementRotationQuantization =>
        ERotatorQuantization.ShortComponents;
    protected override void Configure()
    {
        AddPropertyHandle(10, x => x.ReplicatedMovement)
            .ReplicatedMovement(MovementRotationQuantization);
    }
}
public sealed class ShortFlash : BaseFlash<ShortFlash>
{
    public override string Path => "/short";
}
public sealed class ByteFlash : BaseFlash<ByteFlash>
{
    public override string Path => "/byte";
    protected override ERotatorQuantization MovementRotationQuantization =>
        ERotatorQuantization.ByteComponents;
}
'''})
        self.assertIn('group_path: "/short", field_name: "ReplicatedMovement", field_type: FieldType::RepMovement { rotation: RotatorQuantization::ShortComponents, location: VectorQuantization::RoundWholeNumber }', output)
        self.assertIn('group_path: "/byte", field_name: "ReplicatedMovement", field_type: FieldType::RepMovement { rotation: RotatorQuantization::ByteComponents, location: VectorQuantization::RoundWholeNumber }', output)

    def test_every_movement_form_states_its_location_quantization(self):
        """The explicit, bare and virtual `.ReplicatedMovement` forms all emit
        the location level. The wire does not carry it, so an entry without it
        would leave the reader to assume one -- the constant divisor this
        field exists to replace."""
        output = self.run_generator({"Movement.cs": r'''
public sealed class Explicit : ExportGroupDescriptor<Explicit>
{
    public override string Path => "/explicit";
    protected override void Configure()
    {
        AddProperty(x => x.ReplicatedMovement)
            .ReplicatedMovement(ERotatorQuantization.ByteComponents);
    }
}
public sealed class Bare : ExportGroupDescriptor<Bare>
{
    public override string Path => "/bare";
    protected override void Configure()
    {
        AddProperty(x => x.ReplicatedMovement).ReplicatedMovement();
    }
}
'''})
        types = {
            group: field_type.strip()
            for group, field, field_type in ENTRY_RE.findall(output)
            if field == "ReplicatedMovement"
        }
        self.assertEqual(types, {
            "/explicit": "FieldType::RepMovement { rotation: RotatorQuantization::ByteComponents, location: VectorQuantization::RoundWholeNumber }",
            "/bare": "FieldType::RepMovement { rotation: RotatorQuantization::ShortComponents, location: VectorQuantization::RoundWholeNumber }",
        })
        self.assertIn("use crate::types::{RotatorQuantization, VectorQuantization};", output)

    def test_unresolved_movement_property_fails(self):
        error = self.run_generator_expecting_failure({"Movement.cs": r'''
public sealed class Flash : ExportGroupDescriptor<Flash>
{
    public override string Path => "/flash";
    protected override void Configure()
    {
        AddProperty(x => x.ReplicatedMovement)
            .ReplicatedMovement(MovementRotationQuantization);
    }
}
'''})
        self.assertIn("cannot resolve virtual movement quantization", error)

    def test_static_path_constants_and_cache_factory(self):
        output = self.run_generator({"Factory.cs": r'''
internal static class Paths
{
    public const string Actor = "/actor";
    public const string Rpc = Actor + ":Stop";
}
public sealed class ActorDescriptor : ExportGroupDescriptor<ActorDescriptor>
{
    public override string Path => Paths.Actor;
    protected override void Configure()
    {
        AddProperty(x => x.Value).Float();
    }
}
public sealed class RpcParameters : ExportGroupDescriptor<RpcParameters>
{
    public override string Path => Paths.Rpc;
    protected override void Configure()
    {
        AddProperty(x => x.Flag).Bool();
    }
}
internal static class Factories
{
    public static ClassNetCacheDescriptor CreateStop(string actorPath, uint handle) =>
        new(actorPath + "_ClassNetCache", [
            new RpcDescriptor { Name = "Stop", Handle = handle },
        ]);
}
public static class Catalog
{
    public static void Add() => Factories.CreateStop(Paths.Actor, 3);
}
'''})
        self.assertIn('group_path: "/actor", field_name: "Value"', output)
        self.assertIn('group_path: "/actor:Stop", field_name: "Flag"', output)
        self.assertIn('group_path: "/actor_ClassNetCache", field_name: "Stop", field_type: FieldType::Skip', output)

    def test_unresolved_path_constant_fails(self):
        error = self.run_generator_expecting_failure({"Bad.cs": r'''
public sealed class Bad : ExportGroupDescriptor<Bad>
{
    public override string Path => Missing.Actor;
    protected override void Configure() { AddProperty(x => x.Value).Float(); }
}
'''})
        self.assertIn("unresolved path constant", error)

    def test_descriptor_path_constants_keep_their_class_scope(self):
        output = self.run_generator({"Reveal.cs": r'''
public abstract class Device<T> : ExportGroupDescriptor<T>
{
    protected override void Configure()
    {
        AddPropertyHandle(11, x => x.Owner).ObjectNetGuid();
    }
}
public sealed class Sova : Device<Sova>
{
    public const string DescriptorPath = "/sova";
    public override string Path => DescriptorPath;
}
public sealed class Fade : Device<Fade>
{
    private const string Prefix = "/fade";
    public const string DescriptorPath = Prefix + "/device";
    public override string Path => DescriptorPath;
}
'''})
        for path in ("/sova", "/fade/device"):
            self.assertIn(f'group_path: "{path}", field_name: "Owner", field_type: FieldType::ObjectNetGuid', output)
            self.assertIn(f'group_path: "{path}", handle: 11, field_name: "Owner"', output)

    def test_descriptor_path_cannot_borrow_another_class_constant(self):
        error = self.run_generator_expecting_failure({"Reveal.cs": r'''
public sealed class Sova : ExportGroupDescriptor<Sova>
{
    public const string DescriptorPath = "/sova";
    public override string Path => DescriptorPath;
}
public sealed class Fade : ExportGroupDescriptor<Fade>
{
    public override string Path => DescriptorPath;
}
'''})
        self.assertIn("Fade.DescriptorPath", error)

    def test_a_generic_and_a_plain_class_sharing_a_name_stay_two_classes(self):
        """C# tells `Res` from `Res<T>` by arity, and the vendored tree declares
        both for ResourceComponentDescriptor. Keyed by bare name they merged
        into one record, and file order decided whose fields survived."""
        generic = r'''
public abstract class Res<T> : ExportGroupDescriptor<T> where T : Res<T>
{
    protected override void Configure()
    {
        AddProperty(x => x.BaseValue).Int32();
    }
}
'''
        plain = r'''
public sealed class Res : Res<Res>
{
    public override string Path => "/res";
    protected override void Configure()
    {
        AddProperty(x => x.OwnValue).Float();
    }
}
'''
        for generic_file, plain_file in (("A.cs", "B.cs"), ("B.cs", "A.cs")):
            with self.subTest(generic_file=generic_file):
                output = self.run_generator({generic_file: generic, plain_file: plain})
                self.assertEqual(
                    {(g, f, t.strip()) for g, f, t in ENTRY_RE.findall(output)},
                    {("/res", "BaseValue", "FieldType::Int32"),
                     ("/res", "OwnValue", "FieldType::Float")})

    def test_arity_keys_resolve_own_constants_and_bases(self):
        """Classes are keyed by name and generic arity (`Gen`2`). The arity
        counts top-level type arguments only, and a generic class's constants
        and every base reference use the same key: otherwise `Gen<T>` and
        `Gen<TKey, TSelf>` merge, a Path stops resolving, or a derived class
        loses its base's fields."""
        output = self.run_generator({"Generic.cs": r'''
public class Probe<T> : ExportGroupDescriptor<T>
{
    public const string DescriptorPath = "/probe";
    public override string Path => DescriptorPath;
    protected override void Configure()
    {
        AddProperty(x => x.Value).Int32();
    }
}
public abstract class Gen<T> : ExportGroupDescriptor<T>
{
    protected override void Configure()
    {
        AddProperty(x => x.One).Int32();
    }
}
public abstract class Gen<TKey, TSelf> : ExportGroupDescriptor<TSelf>
{
    protected override void Configure()
    {
        AddProperty(x => x.Two).Float();
    }
}
public sealed class UsesOne : Gen<UsesOne>
{
    public override string Path => "/one";
}
public sealed class UsesTwo : Gen<Dictionary<int, string>, UsesTwo>
{
    public override string Path => "/two";
}
'''})
        self.assertEqual(
            {(g, f, t.strip()) for g, f, t in ENTRY_RE.findall(output)},
            {("/probe", "Value", "FieldType::Int32"),
             ("/one", "One", "FieldType::Int32"),
             ("/two", "Two", "FieldType::Float")})

    def test_unsupported_dynamic_cache_factory_fails(self):
        error = self.run_generator_expecting_failure({"Factory.cs": r'''
internal static class Factories
{
    public static ClassNetCacheDescriptor Build(string path) =>
        new(path, [new RpcDescriptor { Name = "Unsupported" }]);
}
'''})
        self.assertIn("unsupported ClassNetCache factory", error)

    def test_private_cache_factory_fails_instead_of_vanishing(self):
        """Upstream 8b7afcb's ClayDescriptors builds its caches this way; a
        marker that saw only public/internal factories let them go missing."""
        error = self.run_generator_expecting_failure({"ClayDescriptors.cs": r'''
public static class ClayDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> CreateClassNetCacheDescriptors() =>
    [
        Rpc(ClayPaths.Rocket, Function(ClayPaths.Rocket, "MulticastStopProjectile", 3)),
    ];

    private static ClassNetCacheDescriptor Rpc(string path, params RpcDescriptor[] functions) =>
        new(path + "_ClassNetCache", functions);
    private static RpcDescriptor Function(string path, string name, uint handle) => new()
    {
        Name = name, FunctionExportPath = path + ":" + name, Handle = handle,
    };
}
public static class ClayPaths
{
    public const string Rocket = "/Game/Rocket.Rocket_C";
}
'''})
        self.assertIn("unsupported ClassNetCache factory Rpc", error)

    def test_runtime_cache_construction_without_a_factory_list_fails_loudly(self):
        """Upstream 8b7afcb's AgentClassNetCacheDescriptors passes a method
        call where the list goes. Unguarded, every agent cache entry vanished
        from the table and the run still succeeded."""
        error = self.run_generator_expecting_failure({
            "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
public sealed class LiveAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Agents/Live.Live_C";
}
''',
            "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors)
    {
        return agentDescriptors
            .Select(agent => new ClassNetCacheDescriptor(
                agent.Path + "_ClassNetCache",
                CreateFunctions(agent)))
            .ToArray();
    }

    private static IReadOnlyList<RpcDescriptor> CreateFunctions(ExportGroupDescriptor agent)
    {
        return [CreateKillRpc()];
    }

    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = "MulticastNotifyKilledEnemy",
    };
}
''',
        })
        self.assertIn("unsupported runtime ClassNetCache construction", error)

    def test_unsupported_path_override_shape_fails(self):
        error = self.run_generator_expecting_failure({"Bad.cs": r'''
public sealed class Bad : ExportGroupDescriptor<Bad>
{
    public override string Path { get => "/bad"; }
}
'''})
        self.assertIn("unsupported override shape", error)

    def test_source_tree_with_no_csharp_files_is_rejected(self):
        """A missing checkout must not regenerate the overlay as an empty table."""
        result, output = self.run_generator_process({})
        self.assertNotEqual(result.returncode, 0)
        self.assertIsNone(output)
        self.assertIn("no .cs source files", result.stderr)

    def test_sources_that_legitimately_emit_no_entries_still_succeed(self):
        output = self.run_generator(
            {
                "OnlyFastArray.cs": r'''
public sealed class OnlyFastArray : ExportGroupDescriptor<OnlyFastArray>
{
    public override string Path => "/Script/ShooterGame.OnlyFastArray";
    public override ExportGroupKind Kind => ExportGroupKind.FastArray;
    protected override void Configure()
    {
        AddProperty(x => x.Value).Float();
    }
}
'''
            }
        )
        self.assertIn("0 entries from 0 groups", output)

    def test_named_payload_decoders_carry_their_type_through_decode(self):
        """`.Decode(ValorantPayloadDecoders.X(...))` must not collapse to Raw
        when X names a wire type.

        The C# reference moved these fields off direct `.FVectorNetQuantize100()`
        calls onto decoder objects. The generator keyed only on the method name,
        so every one of them became Raw -- and the committed table.rs carries
        them typed, meaning a regeneration would have silently downgraded eight
        entries. That is the hazard docs/archive/PROJECT_STATUS.md section 8
        describes.

        RawPayload stays Raw in the same descriptor, because a decoder whose
        name does not state a type is unknown, not raw.
        """
        output = self.run_generator(
            {
                "Damage.cs": """
public sealed class DamageParameters : ExportGroupDescriptor<DamageParameters>
{
    public override string Path => "/Script/ShooterGame.Damageable:MulticastNotifyDamage";
    protected override void Configure()
    {
        AddProperty(x => x.Origin)
            .Decode(ValorantPayloadDecoders.VectorNetQuantize100("Origin"));
        AddProperty(x => x.Direction).Decode(ValorantPayloadDecoders.VectorNetQuantizeNormal("Direction"));
        AddProperty(x => x.Impact).Decode(ValorantPayloadDecoders.VectorNetQuantize("Impact"));
        AddProperty(x => x.EquippableUsed).Decode(ValorantPayloadDecoders.Equippable);
        AddProperty(x => x.Blob).Decode(ValorantPayloadDecoders.RawPayload("TArray<FThing>"));
    }
}
""",
            }
        )
        got = {
            (field, field_type.strip())
            for group, field, field_type in ENTRY_RE.findall(output)
            if group.endswith(":MulticastNotifyDamage")
        }
        self.assertEqual(
            got,
            {
                ("Origin", "FieldType::VectorNetQuantize { scale: 100 }"),
                ("Direction", "FieldType::VectorNetQuantizeNormal"),
                ("Impact", "FieldType::VectorNetQuantize { scale: 1 }"),
                ("EquippableUsed", "FieldType::ObjectNetGuid"),
                ("Blob", "FieldType::Raw"),
            },
        )

    def test_add_raw_wrapper_calls_emit_all_eleven_raw_fields(self):
        output = self.run_generator(
            {
                "DamageParameters.cs": r'''
public abstract class DamageParameters<T> : ExportGroupDescriptor<T>
{
    protected void AddSharedFields()
    {
        AddRaw(9, x => x.LifeChangeEvents, "LifeChangeEvents");
        AddRaw(11, x => x.LifeResult, "LifeResult");
    }

    protected void AddRaw(uint handle, Expression<Func<T, ValorantRawPayload?>> property, string typeName) =>
        AddPropertyHandle(handle, property, ExportCategory.Gunplay).Decode(RawPayload(typeName));
}
''',
                "MulticastNotifyDamageBaseParameters.cs": r'''
public sealed class MulticastNotifyDamageBaseParameters : DamageParameters<MulticastNotifyDamageBaseParameters>
{
    public override string Path => "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Base";
    protected override void Configure()
    {
        AddSharedFields();
        AddRaw(35, x => x.DeathMontageEffectOverride, "DeathMontageEffectOverride");
        AddRaw(36, x => x.DeathMontageEffectOverrideContext, "DeathMontageEffectOverrideContext");
    }
}
''',
                "MulticastNotifyDamagePointParameters.cs": r'''
public sealed class MulticastNotifyDamagePointParameters : DamageParameters<MulticastNotifyDamagePointParameters>
{
    public override string Path => "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Point";
    protected override void Configure()
    {
        AddSharedFields();
        AddRaw(34, x => x.AssistsList, "AssistsList");
        AddRaw(35, x => x.AssistType, "AssistType");
        AddRaw(38, x => x.AssistTag, "AssistTag");
        AddRaw(43, x => x.DeathMontageEffectOverride, "DeathMontageEffectOverride");
        AddRaw(44, x => x.DeathMontageEffectOverrideContext, "DeathMontageEffectOverrideContext");
    }
}
''',
            }
        )

        entries = {
            (group, field, field_type.strip())
            for group, field, field_type in ENTRY_RE.findall(output)
        }
        self.assertEqual(
            entries,
            {
                (
                    "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Base",
                    "LifeChangeEvents",
                    "FieldType::Raw",
                ),
                (
                    "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Base",
                    "LifeResult",
                    "FieldType::Raw",
                ),
                (
                    "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Base",
                    "DeathMontageEffectOverride",
                    "FieldType::Raw",
                ),
                (
                    "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Base",
                    "DeathMontageEffectOverrideContext",
                    "FieldType::Raw",
                ),
                (
                    "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Point",
                    "LifeChangeEvents",
                    "FieldType::Raw",
                ),
                (
                    "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Point",
                    "LifeResult",
                    "FieldType::Raw",
                ),
                (
                    "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Point",
                    "AssistsList",
                    "FieldType::Raw",
                ),
                (
                    "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Point",
                    "AssistType",
                    "FieldType::Raw",
                ),
                (
                    "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Point",
                    "AssistTag",
                    "FieldType::Raw",
                ),
                (
                    "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Point",
                    "DeathMontageEffectOverride",
                    "FieldType::Raw",
                ),
                (
                    "/Script/ShooterGame.DamageableComponent:MulticastNotifyDamage_Point",
                    "DeathMontageEffectOverrideContext",
                    "FieldType::Raw",
                ),
            },
        )

    def test_called_class_net_cache_helpers_emit_two_skip_entries(self):
        output = self.run_generator(
            {
                "BombGameStateClassNetCacheDescriptor.cs": r'''
public sealed class BombGameStateClassNetCacheDescriptor
    : ClassNetCacheDescriptor<BombGameStateClassNetCacheDescriptor>
{
    public override string Path => "/Game/GameModes/Bomb/BombGameState.BombGameState_C_ClassNetCache";

    protected override void Configure()
    {
        AddTemporaryDeathBase();
        AddTemporaryDeathPoint();
    }

    private void AddTemporaryDeathBase()
    {
        AddFunction<MulticastReceivePlayerTemporaryDeathEventBaseParameters>(
            "MulticastReceivePlayerTemporaryDeathEvent_Base", "/base", ExportCategory.GameState);
    }

    private void AddTemporaryDeathPoint()
    {
        AddFunction<MulticastReceivePlayerTemporaryDeathEventPointParameters>(
            "MulticastReceivePlayerTemporaryDeathEvent_Point", "/point", ExportCategory.GameState);
    }

    private void UnusedHelper()
    {
        AddFunction("MustNotBeExtracted", "/unused", ExportCategory.GameState);
    }
}
'''
            }
        )

        entries = {
            (group, field, field_type.strip())
            for group, field, field_type in ENTRY_RE.findall(output)
        }
        self.assertEqual(
            entries,
            {
                (
                    "/Game/GameModes/Bomb/BombGameState.BombGameState_C_ClassNetCache",
                    "MulticastReceivePlayerTemporaryDeathEvent_Base",
                    "FieldType::Skip",
                ),
                (
                    "/Game/GameModes/Bomb/BombGameState.BombGameState_C_ClassNetCache",
                    "MulticastReceivePlayerTemporaryDeathEvent_Point",
                    "FieldType::Skip",
                ),
            },
        )

    def test_runtime_agent_cache_factory_emits_each_agent_cache(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
    protected override void Configure() { AddProperty(x => x.Owner).ObjectNetGuid(); }
}
''',
                "AlphaAgentDescriptor.cs": r'''
public sealed class AlphaAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Characters/Alpha/Alpha_PC.Alpha_PC_C";
}
''',
                "BetaAgentDescriptor.cs": r'''
public sealed class BetaAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Characters/Beta/Beta_PC.Beta_PC_C";
}
''',
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    private const string KillFunctionName = "MulticastNotifyKilledEnemy";
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(IEnumerable<ExportGroupDescriptor> agentDescriptors)
    {
        return agentDescriptors
            .Select(agent => new ClassNetCacheDescriptor(
                agent.Path + "_ClassNetCache", [CreateKillRpc()]))
            .ToArray();
    }
    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = KillFunctionName,
    };
}
''',
            }
        )

        entries = {
            (group, field, field_type.strip())
            for group, field, field_type in ENTRY_RE.findall(output)
            if group.endswith("_ClassNetCache")
        }
        self.assertEqual(
            entries,
            {
                (
                    "/Game/Characters/Alpha/Alpha_PC.Alpha_PC_C_ClassNetCache",
                    "MulticastNotifyKilledEnemy",
                    "FieldType::Skip",
                ),
                (
                    "/Game/Characters/Beta/Beta_PC.Beta_PC_C_ClassNetCache",
                    "MulticastNotifyKilledEnemy",
                    "FieldType::Skip",
                ),
            },
        )

    def test_runtime_agent_cache_respects_explicit_non_agent_category_override(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
    protected override void Configure() { AddProperty(x => x.Owner).ObjectNetGuid(); }
}
''',
                "AlphaAgentDescriptor.cs": r'''
public sealed class AlphaAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Characters/Alpha/Alpha_PC.Alpha_PC_C";
}
''',
                "HunterDronePawnDescriptor.cs": r'''
public sealed class HunterDronePawnDescriptor : GenericAgentDescriptor
{
    public override string Path =>
        "/Game/Characters/Hunter/Drone/Pawn_Hunter_Drone.Pawn_Hunter_Drone_C";
    public override ExportCategory Categories => ExportCategory.Ability;
}
''',
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    private const string KillFunctionName = "MulticastNotifyKilledEnemy";
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors)
    {
        return agentDescriptors
            .Select(agent => new ClassNetCacheDescriptor(
                agent.Path + "_ClassNetCache", [CreateKillRpc()]))
            .ToArray();
    }
    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = KillFunctionName,
    };
}
''',
            }
        )

        self.assertEqual(
            {
                (group, field, field_type.strip())
                for group, field, field_type in ENTRY_RE.findall(output)
                if group.endswith("_ClassNetCache")
            },
            {
                (
                    "/Game/Characters/Alpha/Alpha_PC.Alpha_PC_C_ClassNetCache",
                    "MulticastNotifyKilledEnemy",
                    "FieldType::Skip",
                ),
            },
        )

    def test_fully_qualified_ability_override_suppresses_runtime_agent_cache(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
    protected override void Configure() { AddProperty(x => x.Owner).ObjectNetGuid(); }
}
''',
                "OrdinaryAgentDescriptor.cs": r'''
public sealed class OrdinaryAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Agents/Ordinary.Ordinary_C";
}
''',
                "QualifiedAbilityDescriptor.cs": r'''
public sealed class QualifiedAbilityDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Abilities/Qualified.Qualified_C";
    private string Display => $"{Format("/*")}";
    public override Replay./* namespace trivia */Models.Descriptors.ExportCategory Categories =>
        global::Replay.Models.Descriptors.ExportCategory /* value trivia */ . Ability;
}
''',
                "AgentClassNetCacheDescriptors.cs": RUNTIME_AGENT_CACHE_FACTORY,
            }
        )

        self.assertEqual(
            self.runtime_cache_entries(output),
            {
                (
                    "/Game/Agents/Ordinary.Ordinary_C_ClassNetCache",
                    "MulticastNotifyKilledEnemy",
                    "FieldType::Skip",
                ),
            },
        )

    def test_alias_category_override_fails_loudly(self):
        error = self.run_generator_expecting_failure(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
''',
                "AliasAbilityDescriptor.cs": r'''
using EC = Replay.Models.Descriptors.ExportCategory;

public sealed class AliasAbilityDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Abilities/Alias.Alias_C";
    public override EC Categories => EC.Ability;
}
''',
                "AgentClassNetCacheDescriptors.cs": RUNTIME_AGENT_CACHE_FACTORY,
            }
        )

        self.assertIn("AliasAbilityDescriptor", error)
        self.assertIn("unsupported ExportCategory override", error)
        self.assertIn("EC", error)

    def test_escaped_alias_and_property_category_override_fails_loudly(self):
        error = self.run_generator_expecting_failure(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
''',
                "EscapedAliasAbilityDescriptor.cs": r'''
using @EC = Replay.Models.Descriptors.ExportCategory;

public sealed class EscapedAliasAbilityDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Abilities/EscapedAlias.EscapedAlias_C";
    public override @EC @Categories => @EC.Ability;
}
''',
                "AgentClassNetCacheDescriptors.cs": RUNTIME_AGENT_CACHE_FACTORY,
            }
        )

        self.assertIn("EscapedAliasAbilityDescriptor", error)
        self.assertIn("unsupported ExportCategory override", error)
        self.assertIn("@EC", error)

    def test_raw_string_category_decoys_preserve_inherited_agent_cache(self):
        variants = (
            (
                "plain",
                r'''
    private const string Decoy = """"
three quotes remain content: """
public override ExportCategory Categories => ExportCategory.Ability;
"""";
''',
                "/Game/RawStrings/Plain.Plain_C",
                "/Game/RawStrings/Plain.Plain_C_ClassNetCache",
            ),
            (
                "interpolated",
                r'''
    private string Decoy => $""""
{Format("\"\"\"")}
public override ExportCategory Categories => ExportCategory.Ability;
"""";
''',
                "/Game/RawStrings/Interpolated.Interpolated_C",
                "/Game/RawStrings/Interpolated.Interpolated_C_ClassNetCache",
            ),
            (
                "double-dollar-four-quotes",
                r'''
    private string Decoy => $$""""
{ "literal": "single braces" }
{{Format("""inner raw""")}}
public override ExportCategory Categories => ExportCategory.Ability;
"""";
''',
                "/Game/RawStrings/DoubleDollar.DoubleDollar_C",
                "/Game/RawStrings/DoubleDollar.DoubleDollar_C_ClassNetCache",
            ),
        )

        for label, declaration, path, expected_group in variants:
            with self.subTest(label=label):
                class_name = f"{label.replace('-', '').title()}AgentDescriptor"
                output = self.run_generator(
                    {
                        "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
''',
                        f"{class_name}.cs": f'''
public sealed class {class_name} : GenericAgentDescriptor
{{
    public override string Path => "{path}";
{declaration}
}}
''',
                        "AgentClassNetCacheDescriptors.cs": RUNTIME_AGENT_CACHE_FACTORY,
                    }
                )

                self.assertEqual(
                    self.runtime_cache_entries(output),
                    {
                        (
                            expected_group,
                            "MulticastNotifyKilledEnemy",
                            "FieldType::Skip",
                        ),
                    },
                )

    def test_nested_category_override_does_not_change_outer_category(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
''',
                "OuterAgentDescriptor.cs": r'''
public sealed class OuterAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Agents/Outer.Outer_C";

    public sealed class NestedAbilityDescriptor : GenericAgentDescriptor
    {
        public override string Path => "/Game/Abilities/Nested.Nested_C";
        public override ExportCategory Categories => ExportCategory.Ability;
    }
}
''',
                "OuterAbilityDescriptor.cs": r'''
public sealed class OuterAbilityDescriptor : GenericAgentDescriptor
{
    public sealed class NestedAgentDescriptor : GenericAgentDescriptor
    {
        public override ExportCategory Categories => ExportCategory.Agent;
    }

    public override string Path => "/Game/Abilities/Outer.Outer_C";
    public override ExportCategory Categories => ExportCategory.Ability;
}
''',
                "AgentClassNetCacheDescriptors.cs": RUNTIME_AGENT_CACHE_FACTORY,
            }
        )

        self.assertEqual(
            self.runtime_cache_entries(output),
            {
                (
                    "/Game/Agents/Outer.Outer_C_ClassNetCache",
                    "MulticastNotifyKilledEnemy",
                    "FieldType::Skip",
                ),
            },
        )

    def test_nested_path_does_not_supply_outer_path(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
''',
                "GenericAbilityDescriptor.cs": r'''
public abstract class GenericAbilityDescriptor : ExportGroupDescriptor<GenericAbilityDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Ability;
}
''',
                "OuterAgentDescriptor.cs": r'''
public sealed class OuterAgentDescriptor : GenericAgentDescriptor
{
    public sealed class NestedAbilityDescriptor : GenericAbilityDescriptor
    {
        public override string Path => "/Game/Abilities/Nested.Nested_C";
        protected override void Configure()
        {
            AddProperty(x => x.NestedSecret).ObjectNetGuid();
        }
    }

    public override string Path => "/Game/Agents/Outer.Outer_C";
    protected override void Configure()
    {
        AddProperty(x => x.OuterOwner).ObjectNetGuid();
    }
}
''',
                "AgentClassNetCacheDescriptors.cs": RUNTIME_AGENT_CACHE_FACTORY,
            }
        )

        descriptor_entries = {
            (group, field, field_type.strip())
            for group, field, field_type in ENTRY_RE.findall(output)
            if not group.endswith("_ClassNetCache")
        }
        self.assertEqual(
            descriptor_entries,
            {
                (
                    "/Game/Abilities/Nested.Nested_C",
                    "NestedSecret",
                    "FieldType::ObjectNetGuid",
                ),
                (
                    "/Game/Agents/Outer.Outer_C",
                    "OuterOwner",
                    "FieldType::ObjectNetGuid",
                ),
            },
        )
        self.assertEqual(
            self.runtime_cache_entries(output),
            {
                (
                    "/Game/Agents/Outer.Outer_C_ClassNetCache",
                    "MulticastNotifyKilledEnemy",
                    "FieldType::Skip",
                ),
            },
        )

    def test_nested_configure_does_not_supply_outer_fields(self):
        output = self.run_generator(
            {
                "OuterDescriptor.cs": r'''
public sealed class OuterDescriptor : ExportGroupDescriptor<OuterDescriptor>
{
    public override string Path => "/Game/Outer.Outer_C";

    public sealed class NestedDescriptor : ExportGroupDescriptor<NestedDescriptor>
    {
        public override string Path => "/Game/Nested.Nested_C";
        protected override void Configure()
        {
            AddProperty(x => x.NestedSecret).ObjectNetGuid();
        }
    }

    protected override void Configure()
    {
        AddProperty(x => x.OuterOwner).ObjectNetGuid();
    }
}
''',
            }
        )

        descriptor_entries = {
            (group, field, field_type.strip())
            for group, field, field_type in ENTRY_RE.findall(output)
        }
        self.assertEqual(
            descriptor_entries,
            {
                (
                    "/Game/Nested.Nested_C",
                    "NestedSecret",
                    "FieldType::ObjectNetGuid",
                ),
                (
                    "/Game/Outer.Outer_C",
                    "OuterOwner",
                    "FieldType::ObjectNetGuid",
                ),
            },
        )

    def test_expression_bodied_configure_stops_before_nested_type(self):
        output = self.run_generator(
            {
                "OuterDescriptor.cs": r'''
public sealed class OuterDescriptor : ExportGroupDescriptor<OuterDescriptor>
{
    public override string Path => "/Game/Outer.Outer_C";
    protected override void Configure() =>
        AddProperty(x => x.OuterOwner).ObjectNetGuid();

    public sealed class NestedDescriptor : ExportGroupDescriptor<NestedDescriptor>
    {
        public override string Path => "/Game/Nested.Nested_C";
        protected override void Configure()
        {
            AddProperty(x => x.NestedSecret).ObjectNetGuid();
        }
    }
}
''',
            }
        )

        descriptor_entries = {
            (group, field, field_type.strip())
            for group, field, field_type in ENTRY_RE.findall(output)
        }
        self.assertEqual(
            descriptor_entries,
            {
                (
                    "/Game/Nested.Nested_C",
                    "NestedSecret",
                    "FieldType::ObjectNetGuid",
                ),
                (
                    "/Game/Outer.Outer_C",
                    "OuterOwner",
                    "FieldType::ObjectNetGuid",
                ),
            },
        )

    def test_category_like_comments_preserve_real_and_inherited_categories(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
    protected override void Configure() { AddProperty(x => x.Owner).ObjectNetGuid(); }
}
''',
                "CommentedInheritedAgentDescriptor.cs": r'''
public sealed class CommentedInheritedAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Comments//Inherited/*literal*/.Inherited_C";
    private const string Regular =
        "escaped \" // public override ExportCategory Categories => ExportCategory.Ability;";
    private const string Verbatim =
        @"escaped "" /* public override ExportCategory Categories => ExportCategory.Ability;";
    private string Interpolated =>
        $"// public override ExportCategory Categories => ExportCategory.Ability;";
    private string InterpolatedVerbatim =>
        $@"/* public override ExportCategory Categories => ExportCategory.Ability;";
    private string VerbatimInterpolated =>
        @$"// public override ExportCategory Categories => ExportCategory.Ability;";
    private const char Slash = '/';
    private const char Quote = '\'';
    // public override ExportCategory Categories => ExportCategory.Ability;
    /*
    public override ExportCategory Categories
    {
        get => ExportCategory.Ability;
    }
    */
}
''',
                "CommentedAbilityDescriptor.cs": r'''
public sealed class CommentedAbilityDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Comments/Ability.Ability_C";
    /* public override ExportCategory Categories => ExportCategory.Agent; */
    // } This comment does not close the class.
    public override ExportCategory Categories => ExportCategory.Ability;
}
''',
                "AgentClassNetCacheDescriptors.cs": RUNTIME_AGENT_CACHE_FACTORY,
            }
        )

        self.assertEqual(
            self.runtime_cache_entries(output),
            {
                (
                    "/Game/Comments//Inherited/*literal*/.Inherited_C_ClassNetCache",
                    "MulticastNotifyKilledEnemy",
                    "FieldType::Skip",
                ),
            },
        )

    def test_agent_flags_and_all_retain_runtime_agent_caches(self):
        output = self.run_generator(
            {
                "GenericAbilityDescriptor.cs": r'''
public abstract class GenericAbilityDescriptor : ExportGroupDescriptor<GenericAbilityDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Ability;
    protected override void Configure() { AddProperty(x => x.Owner).ObjectNetGuid(); }
}
''',
                "AgentAbilityDescriptor.cs": r'''
public sealed class AgentAbilityDescriptor : GenericAbilityDescriptor
{
    public override string Path => "/Game/Flags/AgentAbility.AgentAbility_C";
    public override Replay.Models.Descriptors.ExportCategory Categories =>
        Replay.Models.Descriptors.ExportCategory.Agent |
        Replay.Models.Descriptors.ExportCategory.Ability;
}
''',
                "AllCategoriesDescriptor.cs": r'''
public sealed class AllCategoriesDescriptor : GenericAbilityDescriptor
{
    public override string Path => "/Game/Flags/All.All_C";
    public override Replay.Models.Descriptors.ExportCategory Categories =>
        Replay.Models.Descriptors.ExportCategory.All;
}
''',
                "AgentClassNetCacheDescriptors.cs": RUNTIME_AGENT_CACHE_FACTORY,
            }
        )

        self.assertEqual(
            self.runtime_cache_entries(output),
            {
                (
                    "/Game/Flags/AgentAbility.AgentAbility_C_ClassNetCache",
                    "MulticastNotifyKilledEnemy",
                    "FieldType::Skip",
                ),
                (
                    "/Game/Flags/All.All_C_ClassNetCache",
                    "MulticastNotifyKilledEnemy",
                    "FieldType::Skip",
                ),
            },
        )

    def test_unknown_category_override_fails_loudly(self):
        error = self.run_generator_expecting_failure(
            {
                "UnknownCategoryDescriptor.cs": r'''
public sealed class UnknownCategoryDescriptor : ExportGroupDescriptor<UnknownCategoryDescriptor>
{
    public override string Path => "/test/unknown";
    public override ExportCategory Categories => ExportCategory.Telepathy;
    protected override void Configure() { AddProperty(x => x.Owner).ObjectNetGuid(); }
}
'''
            }
        )

        self.assertIn("UnknownCategoryDescriptor", error)
        self.assertIn("unknown ExportCategory Telepathy", error)

    def test_malformed_category_override_fails_loudly(self):
        error = self.run_generator_expecting_failure(
            {
                "MalformedCategoryDescriptor.cs": r'''
public sealed class MalformedCategoryDescriptor : ExportGroupDescriptor<MalformedCategoryDescriptor>
{
    public override string Path => "/test/malformed";
    public override ExportCategory Categories
    {
        get => ExportCategory.Ability;
    }
    protected override void Configure() { AddProperty(x => x.Owner).ObjectNetGuid(); }
}
'''
            }
        )

        self.assertIn("MalformedCategoryDescriptor", error)
        self.assertIn("unsupported ExportCategory override", error)

    def test_runtime_cache_factory_missing_method_fails_loudly(self):
        error = self.run_generator_expecting_failure(
            {
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors)
    {
        return agentDescriptors
            .Select(agent => new ClassNetCacheDescriptor(
                agent.Path + "_ClassNetCache", [CreateKillRpc()]))
            .ToArray();
    }
}
'''
            }
        )

        self.assertIn("runtime ClassNetCache factory CreateKillRpc", error)
        self.assertIn("method body", error)

    def test_runtime_cache_factory_name_is_bounded_to_its_method(self):
        error = self.run_generator_expecting_failure(
            {
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors)
    {
        return agentDescriptors
            .Select(agent => new ClassNetCacheDescriptor(
                agent.Path + "_ClassNetCache", [CreateKillRpc()]))
            .ToArray();
    }

    private static RpcDescriptor CreateKillRpc()
    {
        return new RpcDescriptor
        {
            FunctionExportPath = "/Script/ShooterGame.Agent:MulticastNotifyKilledEnemy",
        };
    }

    private static RpcDescriptor UnrelatedRpc() => new RpcDescriptor
    {
        Name = "MustNotBeCaptured",
    };
}
'''
            }
        )

        self.assertIn("runtime ClassNetCache factory CreateKillRpc", error)
        self.assertIn("RpcDescriptor.Name", error)

    def test_commented_runtime_cache_factory_is_ignored(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
''',
                "LiveAgentDescriptor.cs": r'''
public sealed class LiveAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Agents/Live.Live_C";
}
''',
                "AgentClassNetCacheDescriptors.cs": RUNTIME_AGENT_CACHE_FACTORY
                + r'''
/*
private static RpcDescriptor CreateBogusRpc() => new RpcDescriptor
{
    Name = "MustNotBeCaptured",
};

private static void CreateBogusCaches(
    IEnumerable<ExportGroupDescriptor> agentDescriptors)
{
    _ = agentDescriptors.Select(agent => new ClassNetCacheDescriptor(
        agent.Path + "_Bogus_ClassNetCache", [CreateBogusRpc()]));
}
*/
''',
            }
        )

        self.assertEqual(
            self.runtime_cache_entries(output),
            {
                (
                    "/Game/Agents/Live.Live_C_ClassNetCache",
                    "MulticastNotifyKilledEnemy",
                    "FieldType::Skip",
                ),
            },
        )

    def test_runtime_cache_factory_syntax_inside_string_is_ignored(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
''',
                "LiveAgentDescriptor.cs": r'''
public sealed class LiveAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Agents/Live.Live_C";
}
''',
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    private const string KillFunctionName = "MulticastNotifyKilledEnemy";
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors)
    {
        return agentDescriptors
            .Select(agent => new ClassNetCacheDescriptor(
                agent.Path + "_ClassNetCache", [CreateKillRpc()]))
            .ToArray();
    }
    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = KillFunctionName,
    };

    private static string BuildDiagnostic(GenericAgentDescriptor agent)
    {
        return agent.Path + "_Bogus_ClassNetCache\", [CreateBogusRpc()]";
    }
    private static RpcDescriptor CreateBogusRpc() => new RpcDescriptor
    {
        Name = "MustNotBeCaptured",
    };
}
''',
            }
        )

        runtime_entries = {
            (group, field, field_type.strip())
            for group, field, field_type in ENTRY_RE.findall(output)
            if field in {"MulticastNotifyKilledEnemy", "MustNotBeCaptured"}
        }
        self.assertEqual(
            runtime_entries,
            {
                (
                    "/Game/Agents/Live.Live_C_ClassNetCache",
                    "MulticastNotifyKilledEnemy",
                    "FieldType::Skip",
                ),
            },
        )

    def test_commented_raw_wrapper_does_not_reclassify_live_typed_call(self):
        # This fixture also defined the live typed AddValue as
        # `=> AddPropertyHandle(handle, property, ExportCategory.GameState);`.
        # That body reaches the end of the type ladder with no type method and
        # was dropped without a word; it is now a `<no type method>` rejection,
        # which is right -- the splitter cannot see calls through such a
        # wrapper, so the definition is their only trace. The case moved to
        # test_a_typed_wrapper_definition_is_rejected, which also fails if a
        # typed wrapper is ever misread as a raw one.
        output = self.run_generator(
            {
                "LiveTypedDescriptor.cs": r'''
/*
private void AddValue(uint handle, Expression<Func<object, object>> property) =>
    AddPropertyHandle(handle, property, ExportCategory.GameState).Decode(RawPayload("old"));
*/
public sealed class LiveTypedDescriptor : ExportGroupDescriptor<LiveTypedDescriptor>
{
    public override string Path => "/test/live-typed";
    protected override void Configure()
    {
        AddProperty(x => x.KnownValue).UInt32();
        AddValue(7, x => x.TypedValue).UInt32();
    }
}
'''
            }
        )

        self.assertEqual(
            {
                (group, field, field_type.strip())
                for group, field, field_type in ENTRY_RE.findall(output)
            },
            {
                ("/test/live-typed", "KnownValue", "FieldType::UInt32"),
            },
        )
        self.assertEqual(HANDLE_ENTRY_RE.findall(output), [])

    def test_raw_wrapper_name_does_not_leak_to_unrelated_class(self):
        output = self.run_generator(
            {
                "RawWrapperOwner.cs": r'''
public abstract class RawWrapperOwner<T> : ExportGroupDescriptor<T>
{
    protected void AddValue(
        uint handle,
        Expression<Func<T, ValorantRawPayload?>> property) =>
        AddPropertyHandle(handle, property, ExportCategory.Effects).Decode(RawPayload("raw"));
}
''',
                "UnrelatedDescriptor.cs": r'''
public sealed class UnrelatedDescriptor : ExportGroupDescriptor<UnrelatedDescriptor>
{
    public override string Path => "/test/unrelated";
    protected override void Configure()
    {
        AddProperty(x => x.KnownValue).UInt32();
        AddValue(7, x => x.TypedValue).UInt32();
    }
}
''',
            }
        )

        self.assertEqual(
            {
                (group, field, field_type.strip())
                for group, field, field_type in ENTRY_RE.findall(output)
            },
            {
                ("/test/unrelated", "KnownValue", "FieldType::UInt32"),
            },
        )
        self.assertEqual(HANDLE_ENTRY_RE.findall(output), [])

    def test_duplicate_raw_wrapper_owner_class_name_fails_loudly(self):
        error = self.run_generator_expecting_failure(
            {
                "SharedBaseDescriptors.cs": r'''
namespace RawSide
{
    public abstract class SharedBase<T> : ExportGroupDescriptor<T>
    {
        protected void AddValue(
            uint handle,
            Expression<Func<T, ValorantRawPayload?>> property,
            string typeName) =>
            AddPropertyHandle(handle, property, ExportCategory.Effects)
                .Decode(RawPayload(typeName));
    }
}

namespace TypedSide
{
    public abstract class SharedBase<T> : ExportGroupDescriptor<T>
    {
        protected PropertyDescriptor AddValue(
            uint handle,
            Expression<Func<T, uint>> property) =>
            AddPropertyHandle(handle, property, ExportCategory.GameState);
    }

    public sealed class TypedDescriptor : SharedBase<TypedDescriptor>
    {
        public override string Path => "/test/typed-side";
        protected override void Configure()
        {
            AddValue(7, x => x.TypedValue).UInt32();
        }
    }
}
'''
            }
        )

        self.assertIn("ambiguous raw-wrapper owner SharedBase", error)
        self.assertIn("duplicate class declarations", error)

    def test_commented_raw_wrapper_call_is_not_emitted(self):
        output = self.run_generator(
            {
                "RawBaseDescriptor.cs": r'''
public abstract class RawBaseDescriptor<T> : ExportGroupDescriptor<T>
{
    protected void AddRaw(
        uint handle,
        Expression<Func<T, ValorantRawPayload?>> property,
        string typeName) =>
        AddPropertyHandle(handle, property, ExportCategory.Effects).Decode(RawPayload(typeName));
}
''',
                "LiveRawDescriptor.cs": r'''
public sealed class LiveRawDescriptor : RawBaseDescriptor<LiveRawDescriptor>
{
    public override string Path => "/test/live-raw";
    protected override void Configure()
    {
        AddRaw(7, x => x.LivePayload, "LivePayload");
        /*
        AddRaw(99, x => x.CommentedPayload, "CommentedPayload");
        */
    }
}
''',
            }
        )

        self.assertEqual(
            {
                (group, field, field_type.strip())
                for group, field, field_type in ENTRY_RE.findall(output)
            },
            {
                ("/test/live-raw", "LivePayload", "FieldType::Raw"),
            },
        )
        self.assertEqual(
            {
                (group, int(handle), field)
                for group, handle, field in HANDLE_ENTRY_RE.findall(output)
            },
            {("/test/live-raw", 7, "LivePayload")},
        )

    def test_property_type_syntax_inside_trivia_is_ignored(self):
        output = self.run_generator(
            {
                "TriviaDescriptor.cs": r'''
public sealed class TriviaDescriptor : ExportGroupDescriptor<TriviaDescriptor>
{
    public override string Path => "/test/trivia";
    protected override void Configure()
    {
        AddProperty(x => x.CommentDecode /* .Decode( */).UInt32();
        AddProperty(x => x.CommentSerialized /* .SerializedInt(999) */).UInt32();
        AddProperty("Literal).Decode(", x => x.LiteralValue).UInt32();
    }
}
'''
            }
        )

        self.assertEqual(
            {
                (group, field, field_type.strip())
                for group, field, field_type in ENTRY_RE.findall(output)
            },
            {
                ("/test/trivia", "CommentDecode", "FieldType::UInt32"),
                ("/test/trivia", "CommentSerialized", "FieldType::UInt32"),
                ("/test/trivia", "Literal).Decode(", "FieldType::UInt32"),
            },
        )

    def test_property_name_syntax_inside_comments_is_ignored(self):
        output = self.run_generator(
            {
                "NameTriviaDescriptor.cs": r'''
public sealed class NameTriviaDescriptor : ExportGroupDescriptor<NameTriviaDescriptor>
{
    public override string Path => "/test/name-trivia";
    protected override void Configure()
    {
        AddProperty(/* AddProperty("Wrong") */ x => x.Right).UInt32();
        AddProperty(/* z => z.Decoy */ x => x.Live).UInt32();
        AddPropertyHandle(
            7,
            /* AddPropertyHandle(8, "WrongHandle", ExportCategory.Debug) */
            x => x.RightHandle,
            ExportCategory.Debug).UInt32();
    }
}
'''
            }
        )

        self.assertEqual(
            {
                (group, field, field_type.strip())
                for group, field, field_type in ENTRY_RE.findall(output)
            },
            {
                ("/test/name-trivia", "Right", "FieldType::UInt32"),
                ("/test/name-trivia", "Live", "FieldType::UInt32"),
                ("/test/name-trivia", "RightHandle", "FieldType::UInt32"),
            },
        )
        self.assertEqual(
            {
                (group, int(handle), field)
                for group, handle, field in HANDLE_ENTRY_RE.findall(output)
            },
            {("/test/name-trivia", 7, "RightHandle")},
        )

    def test_escaped_raw_wrapper_call_emits_literal_handle(self):
        output = self.run_generator(
            {
                "EscapedRawBase.cs": r'''
public abstract class EscapedRawBase<T> : ExportGroupDescriptor<T>
{
    protected void @AddRaw(
        uint handle,
        Expression<Func<T, ValorantRawPayload?>> property,
        string typeName) =>
        AddPropertyHandle(handle, property, ExportCategory.Effects).Decode(RawPayload(typeName));
}
''',
                "EscapedRawDescriptor.cs": r'''
public sealed class EscapedRawDescriptor : EscapedRawBase<EscapedRawDescriptor>
{
    public override string Path => "/test/escaped-raw";
    protected override void Configure()
    {
        @AddRaw(7, @x => @x.@Payload, "Payload");
    }
}
''',
            }
        )

        self.assertEqual(
            {
                (group, field, field_type.strip())
                for group, field, field_type in ENTRY_RE.findall(output)
            },
            {("/test/escaped-raw", "Payload", "FieldType::Raw")},
        )
        self.assertEqual(
            {
                (group, int(handle), field)
                for group, handle, field in HANDLE_ENTRY_RE.findall(output)
            },
            {("/test/escaped-raw", 7, "Payload")},
        )

    def test_runtime_factory_resolves_within_owning_class(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
''',
                "LiveAgentDescriptor.cs": r'''
public sealed class LiveAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Agents/Live.Live_C";
}
''',
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class UnrelatedRpcFactory
{
    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = "WrongRpc",
    };
}

internal static class AgentClassNetCacheDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors)
    {
        return agentDescriptors
            .Select(agent => new ClassNetCacheDescriptor(
                agent.Path + "_ClassNetCache", [CreateKillRpc()]))
            .ToArray();
    }

    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = "RightRpc",
    };
}
''',
            }
        )

        self.assertEqual(
            self.runtime_cache_entries(output),
            {
                (
                    "/Game/Agents/Live.Live_C_ClassNetCache",
                    "RightRpc",
                    "FieldType::Skip",
                ),
            },
        )

    def test_runtime_factory_constant_resolves_within_owning_class(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
''',
                "LiveAgentDescriptor.cs": r'''
public sealed class LiveAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Agents/Live.Live_C";
}
''',
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    private const string RpcName = "RightRpc";

    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors)
    {
        return agentDescriptors
            .Select(agent => new ClassNetCacheDescriptor(
                agent.Path + "_ClassNetCache", [CreateKillRpc()]))
            .ToArray();
    }

    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = RpcName,
    };
}

internal static class UnrelatedConstants
{
    private const string RpcName = "WrongRpc";
}
''',
            }
        )

        self.assertEqual(
            self.runtime_cache_entries(output),
            {
                (
                    "/Game/Agents/Live.Live_C_ClassNetCache",
                    "RightRpc",
                    "FieldType::Skip",
                ),
            },
        )

    def test_runtime_factory_name_expression_fails_loudly(self):
        error = self.run_generator_expecting_failure(
            {
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors) =>
        agentDescriptors.Select(agent => new ClassNetCacheDescriptor(
            agent.Path + "_ClassNetCache", [CreateKillRpc()])).ToArray();

    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = "Right" + "Rpc",
    };
}
'''
            }
        )

        self.assertIn("runtime ClassNetCache factory CreateKillRpc", error)
        self.assertIn("unsupported RpcDescriptor.Name initializer", error)

    def test_runtime_factory_constant_expression_fails_loudly(self):
        error = self.run_generator_expecting_failure(
            {
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    private const string RpcName = "Right" + "Rpc";

    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors) =>
        agentDescriptors.Select(agent => new ClassNetCacheDescriptor(
            agent.Path + "_ClassNetCache", [CreateKillRpc()])).ToArray();

    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = RpcName,
    };
}
'''
            }
        )

        self.assertIn("runtime ClassNetCache factory CreateKillRpc", error)
        self.assertIn("unsupported constant RpcName initializer", error)

    def test_runtime_factory_uses_only_returned_descriptor_name(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
''',
                "LiveAgentDescriptor.cs": r'''
public sealed class LiveAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Agents/Live.Live_C";
}
''',
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors) =>
        agentDescriptors.Select(agent => new ClassNetCacheDescriptor(
            agent.Path + "_ClassNetCache", [CreateKillRpc()])).ToArray();

    private static RpcDescriptor CreateKillRpc()
    {
        OtherDescriptor BuildDiagnostic() => new OtherDescriptor
        {
            Name = "WrongRpc",
        };

        _ = BuildDiagnostic;
        return new RpcDescriptor
        {
            Name = "RightRpc",
        };
    }
}
''',
            }
        )

        self.assertEqual(
            self.runtime_cache_entries(output),
            {
                (
                    "/Game/Agents/Live.Live_C_ClassNetCache",
                    "RightRpc",
                    "FieldType::Skip",
                ),
            },
        )

    def test_nested_local_return_cannot_supply_runtime_name(self):
        error = self.run_generator_expecting_failure(
            {
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors) =>
        agentDescriptors.Select(agent => new ClassNetCacheDescriptor(
            agent.Path + "_ClassNetCache", [CreateKillRpc()])).ToArray();

    private static RpcDescriptor CreateKillRpc()
    {
        RpcDescriptor Decoy()
        {
            return new RpcDescriptor { Name = "WrongRpc" };
        }

        _ = Decoy;
        var result = new RpcDescriptor { Name = "RightRpc" };
        return result;
    }
}
'''
            }
        )

        self.assertIn("runtime ClassNetCache factory CreateKillRpc", error)
        self.assertIn("returned RpcDescriptor initializer not found", error)

    def test_nested_local_constant_does_not_shadow_owning_class_constant(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
''',
                "LiveAgentDescriptor.cs": r'''
public sealed class LiveAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Agents/Live.Live_C";
}
''',
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    private const string RpcName = "RightRpc";

    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors) =>
        agentDescriptors.Select(agent => new ClassNetCacheDescriptor(
            agent.Path + "_ClassNetCache", [CreateKillRpc()])).ToArray();

    private static RpcDescriptor CreateKillRpc()
    {
        void BuildDiagnostic()
        {
            const string RpcName = "WrongRpc";
            _ = RpcName;
        }

        _ = BuildDiagnostic;
        return new RpcDescriptor { Name = RpcName };
    }
}
''',
            }
        )

        self.assertEqual(
            self.runtime_cache_entries(output),
            {
                (
                    "/Game/Agents/Live.Live_C_ClassNetCache",
                    "RightRpc",
                    "FieldType::Skip",
                ),
            },
        )

    def test_direct_local_constant_shadow_fails_loudly(self):
        error = self.run_generator_expecting_failure(
            {
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    private const string RpcName = "ClassRpc";

    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors) =>
        agentDescriptors.Select(agent => new ClassNetCacheDescriptor(
            agent.Path + "_ClassNetCache", [CreateKillRpc()])).ToArray();

    private static RpcDescriptor CreateKillRpc()
    {
        const string RpcName = "LocalRpc";
        return new RpcDescriptor { Name = RpcName };
    }
}
'''
            }
        )

        self.assertIn("runtime ClassNetCache factory CreateKillRpc", error)
        self.assertIn("local constant RpcName shadows direct member", error)

    def test_local_runtime_factory_shadow_fails_loudly(self):
        error = self.run_generator_expecting_failure(
            {
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors)
    {
        RpcDescriptor CreateKillRpc() => new RpcDescriptor
        {
            Name = "LocalRpc",
        };

        return agentDescriptors.Select(agent => new ClassNetCacheDescriptor(
            agent.Path + "_ClassNetCache", [CreateKillRpc()])).ToArray();
    }

    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = "ClassRpc",
    };
}
'''
            }
        )

        self.assertIn("runtime ClassNetCache factory CreateKillRpc", error)
        self.assertIn("local factory shadows", error)

    def test_local_factory_delegate_shadow_fails_loudly(self):
        error = self.run_generator_expecting_failure(
            {
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors)
    {
        Func<RpcDescriptor> CreateKillRpc = () => new RpcDescriptor
        {
            Name = "LocalRpc",
        };

        return agentDescriptors.Select(agent => new ClassNetCacheDescriptor(
            agent.Path + "_ClassNetCache", [CreateKillRpc()])).ToArray();
    }

    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = "ClassRpc",
    };
}
'''
            }
        )

        self.assertIn("runtime ClassNetCache factory CreateKillRpc", error)
        self.assertIn("local factory delegate shadows direct member", error)

    def test_comment_bracket_cannot_truncate_runtime_factory_list(self):
        error = self.run_generator_expecting_failure(
            {
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors) =>
        agentDescriptors.Select(agent => new ClassNetCacheDescriptor(
            agent.Path + "_ClassNetCache",
            [FirstRpc() /* ] */, SecondRpc()])).ToArray();

    private static RpcDescriptor FirstRpc() =>
        new RpcDescriptor { Name = "FirstRpc" };
    private static RpcDescriptor SecondRpc() =>
        new RpcDescriptor { Name = "SecondRpc" };
}
'''
            }
        )

        self.assertIn("runtime ClassNetCache", error)
        self.assertIn("unsupported factory list", error)

    def test_multiple_or_trailing_runtime_factories_fail_loudly(self):
        factory_lists = ("[FirstRpc(), SecondRpc()]", "[FirstRpc(),]")
        for factory_list in factory_lists:
            with self.subTest(factory_list=factory_list):
                error = self.run_generator_expecting_failure(
                    {
                        "AgentClassNetCacheDescriptors.cs": rf'''
internal static class AgentClassNetCacheDescriptors
{{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors) =>
        agentDescriptors.Select(agent => new ClassNetCacheDescriptor(
            agent.Path + "_ClassNetCache", {factory_list})).ToArray();

    private static RpcDescriptor FirstRpc() => new RpcDescriptor {{ Name = "First" }};
    private static RpcDescriptor SecondRpc() => new RpcDescriptor {{ Name = "Second" }};
}}
'''
                    }
                )

                self.assertIn("runtime ClassNetCache", error)
                self.assertIn("unsupported factory list", error)

    def test_escaped_runtime_descriptor_type_is_discovered(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
''',
                "LiveAgentDescriptor.cs": r'''
public sealed class LiveAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Agents/Live.Live_C";
}
''',
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors) =>
        agentDescriptors.Select(agent => new @ClassNetCacheDescriptor(
            agent.Path + "_ClassNetCache", [CreateKillRpc()])).ToArray();

    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = "RightRpc",
    };
}
''',
            }
        )

        self.assertEqual(
            self.runtime_cache_entries(output),
            {
                (
                    "/Game/Agents/Live.Live_C_ClassNetCache",
                    "RightRpc",
                    "FieldType::Skip",
                ),
            },
        )

    def test_duplicate_runtime_factory_in_owning_class_fails_loudly(self):
        error = self.run_generator_expecting_failure(
            {
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors)
    {
        return agentDescriptors
            .Select(agent => new ClassNetCacheDescriptor(
                agent.Path + "_ClassNetCache", [CreateKillRpc()]))
            .ToArray();
    }

    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = "FirstRpc",
    };

    private static RpcDescriptor CreateKillRpc() => new RpcDescriptor
    {
        Name = "SecondRpc",
    };
}
'''
            }
        )

        self.assertIn("runtime ClassNetCache factory CreateKillRpc", error)
        self.assertIn("ambiguous", error)

    def test_multiple_runtime_factory_names_fail_loudly(self):
        error = self.run_generator_expecting_failure(
            {
                "AgentClassNetCacheDescriptors.cs": r'''
internal static class AgentClassNetCacheDescriptors
{
    public static IReadOnlyList<ClassNetCacheDescriptor> Create(
        IEnumerable<ExportGroupDescriptor> agentDescriptors)
    {
        return agentDescriptors
            .Select(agent => new ClassNetCacheDescriptor(
                agent.Path + "_ClassNetCache", [CreateKillRpc()]))
            .ToArray();
    }

    private static RpcDescriptor CreateKillRpc() => UseFirst
        ? new RpcDescriptor { Name = "FirstRpc" }
        : new RpcDescriptor { Name = "SecondRpc" };
}
'''
            }
        )

        self.assertIn("runtime ClassNetCache factory CreateKillRpc", error)
        self.assertIn("ambiguous RpcDescriptor", error)

    def test_escaped_nested_class_is_discovered_and_scoped(self):
        output = self.run_generator(
            {
                "OuterDescriptor.cs": r'''
public abstract class @BaseDescriptor : ExportGroupDescriptor<@BaseDescriptor>
{
    protected override void Configure()
    {
        AddProperty(x => x.InheritedValue).UInt32();
    }
}

public sealed class OuterDescriptor : BaseDescriptor
{
    public override string Path => "/outer";
    protected override void Configure()
    {
        AddProperty(x => x.OuterValue).UInt32();
    }

    public sealed class @NestedDescriptor : @BaseDescriptor
    {
        public override string Path => "/nested";
        protected override void Configure()
        {
            AddProperty(x => x.NestedSecret).UInt32();
        }
    }
}
'''
            }
        )

        self.assertEqual(
            {
                (group, field, field_type.strip())
                for group, field, field_type in ENTRY_RE.findall(output)
            },
            {
                ("/outer", "InheritedValue", "FieldType::UInt32"),
                ("/outer", "OuterValue", "FieldType::UInt32"),
                ("/nested", "InheritedValue", "FieldType::UInt32"),
                ("/nested", "NestedSecret", "FieldType::UInt32"),
            },
        )

    def test_unrelated_path_and_factory_arguments_do_not_create_runtime_cache(self):
        output = self.run_generator(
            {
                "GenericAgentDescriptor.cs": r'''
public abstract class GenericAgentDescriptor : ExportGroupDescriptor<GenericAgentDescriptor>
{
    public override ExportCategory Categories => ExportCategory.Agent;
}
''',
                "LiveAgentDescriptor.cs": r'''
public sealed class LiveAgentDescriptor : GenericAgentDescriptor
{
    public override string Path => "/Game/Agents/Live.Live_C";
}
''',
                "AgentDiagnostics.cs": r'''
internal static class AgentDiagnostics
{
    public static void Audit(GenericAgentDescriptor agent)
    {
        Log(agent.Path + "_Audit_ClassNetCache", [CreateAuditRpc()]);
    }

    private static RpcDescriptor CreateAuditRpc() => new RpcDescriptor
    {
        Name = "AuditRpc",
    };
}
''',
            }
        )

        self.assertEqual(self.runtime_cache_entries(output), set())

    def test_literal_property_handles_emit_handle_metadata(self):
        output = self.run_generator(
            {
                "ReplayParameters.cs": r'''
public sealed class ReplayParameters : ExportGroupDescriptor<ReplayParameters>
{
    public override string Path => "/Script/ShooterGame.ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation";
    protected override void Configure()
    {
        AddPropertyHandle(26, x => x.Location, ExportCategory.Effects).FVector();
        AddPropertyHandle(27, x => x.Rotation, ExportCategory.Effects).FRotatorShort();
        AddProperty(x => x.StartMovementTime, ExportCategory.Effects).Float();
    }
}
'''
            }
        )

        self.assertEqual(
            {
                (group, int(handle), field)
                for group, handle, field in HANDLE_ENTRY_RE.findall(output)
            },
            {
                (
                    "/Script/ShooterGame.ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation",
                    26,
                    "Location",
                ),
                (
                    "/Script/ShooterGame.ReplayEffectComponent:ReplayPlayContinuousEffectAtLocation",
                    27,
                    "Rotation",
                ),
            },
        )

    def test_decoder_arguments_do_not_emit_handle_metadata(self):
        output = self.run_generator(
            {
                "BoundedPayloadDescriptor.cs": r'''
public sealed class BoundedPayloadDescriptor : ExportGroupDescriptor<BoundedPayloadDescriptor>
{
    public override string Path => "/Game/Effects/Bounded.Bounded_C";
    protected override void Configure()
    {
        AddProperty(x => x.Payload).Decode(SomeDecoder.WithBounds(9, 10));
    }
}
'''
            }
        )

        self.assertEqual(HANDLE_ENTRY_RE.findall(output), [])
        self.assertIn(
            (
                "/Game/Effects/Bounded.Bounded_C",
                "Payload",
                "FieldType::Raw",
            ),
            {
                (group, field, field_type.strip())
                for group, field, field_type in ENTRY_RE.findall(output)
            },
        )


    # ---- ExportGroupKind ----
    #
    # A descriptor's ExportGroupKind decides whether its properties can be wire
    # field names. Two kinds say no, and every other kind must say yes --
    # including Unknown, which is the C# default from the protected
    # parameterless constructor and NOT a "we did not look" marker.

    def test_fast_array_descriptor_contributes_nothing(self):
        output = self.run_generator(
            {
                "RemoteCharacterUpdateDescriptor.cs": r'''
public sealed class RemoteCharacterUpdateDescriptor : ExportGroupDescriptor<RemoteCharacterUpdateDescriptor>
{
    public override string Path => "/Script/ShooterGame.RemoteCharacterUpdate";
    public override ExportGroupKind Kind => ExportGroupKind.FastArray;
    protected override void Configure()
    {
        AddProperty(x => x.ShooterCharacterNetGuidValue).UInt32();
        AddProperty(x => x.ShooterCharacterNetGuid).ObjectNetGuid();
    }
}
'''
            }
        )

        self.assertEqual(self.entries(output), set())
        # The group must not survive in the header's group count either --
        # a group with no entries would still be counted as covered.
        self.assertIn("0 entries from 0 groups.", output)

    def test_fast_array_kind_is_inherited_by_a_derived_descriptor(self):
        output = self.run_generator(
            {
                "Elements.cs": r'''
public abstract class FastArrayElementDescriptor<T> : ExportGroupDescriptor<T>
{
    public override ExportGroupKind Kind => ExportGroupKind.FastArray;
}

public sealed class DerivedElementDescriptor : FastArrayElementDescriptor<DerivedElementDescriptor>
{
    public override string Path => "/Script/ShooterGame.DerivedElement";
    protected override void Configure()
    {
        AddProperty(x => x.Value).Float();
    }
}
'''
            }
        )

        self.assertEqual(self.entries(output), set())

    def test_attribute_set_keeps_only_the_replicated_pair(self):
        output = self.run_generator(
            {
                "AresAttributeSetDescriptor.cs": r'''
public sealed class AresAttributeSetDescriptor : ExportGroupDescriptor<AresAttributeSetDescriptor>
{
    public override string Path => "/Script/ShooterGame.AresAttributeSet";
    public override ExportGroupKind Kind => ExportGroupKind.AttributeSet;
    protected override void Configure()
    {
        AddProperty(x => x.Health, ExportCategory.Ability).Float();
        AddProperty(x => x.MaxHealth, ExportCategory.Ability).Float();
        AddProperty(x => x.Shield, ExportCategory.Ability).Float();
        AddProperty(x => x.MaxShield, ExportCategory.Ability).Float();
        AddProperty(x => x.Healing, ExportCategory.Ability).Float();
        AddProperty(x => x.Damage, ExportCategory.Ability).Float();
        AddProperty(x => x.BaseValue, ExportCategory.Ability).Float();
        AddProperty(x => x.CurrentValue, ExportCategory.Ability).Float();
    }
}
'''
            }
        )

        self.assertEqual(
            self.entries(output),
            {
                ("/Script/ShooterGame.AresAttributeSet", "BaseValue"),
                ("/Script/ShooterGame.AresAttributeSet", "CurrentValue"),
            },
        )

    def test_attribute_set_without_the_replicated_pair_fails(self):
        stderr = self.run_generator_expecting_failure(
            {
                "RenamedAttributeSetDescriptor.cs": r'''
public sealed class RenamedAttributeSetDescriptor : ExportGroupDescriptor<RenamedAttributeSetDescriptor>
{
    public override string Path => "/Script/ShooterGame.RenamedAttributeSet";
    public override ExportGroupKind Kind => ExportGroupKind.AttributeSet;
    protected override void Configure()
    {
        AddProperty(x => x.Health, ExportCategory.Ability).Float();
    }
}
'''
            }
        )

        self.assertIn("RenamedAttributeSetDescriptor", stderr)
        self.assertIn("BaseValue", stderr)

    def test_descriptor_that_declares_no_kind_keeps_every_field(self):
        """Four live descriptors never override Kind, so they resolve to the
        C# default. Unknown must emit, or their fields vanish silently."""
        output = self.run_generator(
            {
                "SmokeScreenManagerDescriptor.cs": r'''
public sealed class SmokeScreenManagerDescriptor : ExportGroupDescriptor<SmokeScreenManagerDescriptor>
{
    public override string Path => "/Game/Characters/Pandemic/Manager.Manager_C";
    protected override void Configure()
    {
        AddProperty(x => x.Owner).ObjectNetGuid();
        AddProperty(x => x.CurrentFuelLevel).Float();
    }
}
'''
            }
        )

        self.assertEqual(
            self.entries(output),
            {
                ("/Game/Characters/Pandemic/Manager.Manager_C", "Owner"),
                (
                    "/Game/Characters/Pandemic/Manager.Manager_C",
                    "CurrentFuelLevel",
                ),
            },
        )

    def test_explicitly_unknown_kind_keeps_every_field(self):
        output = self.run_generator(
            {
                "BaseReplayPlayerState.cs": r'''
public sealed class BaseReplayPlayerState : ExportGroupDescriptor<BaseReplayPlayerState>
{
    public override string Path => "/Game/GameModes/Common/BaseReplayPlayerState.BaseReplayPlayerState_C";
    public override ExportGroupKind Kind => ExportGroupKind.Unknown;
    protected override void Configure()
    {
        AddProperty(x => x.Owner).ObjectNetGuid();
        AddProperty(x => x.bOnlySpectator).Bool();
    }
}
'''
            }
        )

        self.assertEqual(
            self.entries(output),
            {
                (
                    "/Game/GameModes/Common/BaseReplayPlayerState.BaseReplayPlayerState_C",
                    "Owner",
                ),
                (
                    "/Game/GameModes/Common/BaseReplayPlayerState.BaseReplayPlayerState_C",
                    "bOnlySpectator",
                ),
            },
        )

    def test_each_emitting_kind_keeps_every_field(self):
        for kind in ("Actor", "PlayerController", "Component", "ClassNetCache"):
            with self.subTest(kind=kind):
                output = self.run_generator(
                    {
                        "KindDescriptor.cs": f'''
public sealed class KindDescriptor : ExportGroupDescriptor<KindDescriptor>
{{
    public override string Path => "/Script/ShooterGame.Kinded";
    public override ExportGroupKind Kind => ExportGroupKind.{kind};
    protected override void Configure()
    {{
        AddProperty(x => x.Health).Float();
        AddProperty(x => x.BaseValue).Float();
    }}
}}
'''
                    }
                )

                self.assertEqual(
                    self.entries(output),
                    {
                        ("/Script/ShooterGame.Kinded", "Health"),
                        ("/Script/ShooterGame.Kinded", "BaseValue"),
                    },
                )

    def test_unhandled_kind_is_a_hard_failure(self):
        """A new C# enum member must be classified by a human. Defaulting it
        to "emit" would ship dead entries; defaulting it to "drop" would
        delete live ones. Neither is safe, so neither is the default."""
        stderr = self.run_generator_expecting_failure(
            {
                "FutureDescriptor.cs": r'''
public sealed class FutureDescriptor : ExportGroupDescriptor<FutureDescriptor>
{
    public override string Path => "/Script/ShooterGame.Future";
    public override ExportGroupKind Kind => ExportGroupKind.SparseDelta;
    protected override void Configure()
    {
        AddProperty(x => x.Value).Float();
    }
}
'''
            }
        )

        self.assertIn("FutureDescriptor", stderr)
        self.assertIn("SparseDelta", stderr)

    def test_unsupported_kind_override_shape_is_a_hard_failure(self):
        """Reading an unparseable override as absent would resolve the class to
        Unknown, which emits everything -- a failure shaped like success."""
        stderr = self.run_generator_expecting_failure(
            {
                "ComputedKindDescriptor.cs": r'''
public sealed class ComputedKindDescriptor : ExportGroupDescriptor<ComputedKindDescriptor>
{
    public override string Path => "/Script/ShooterGame.Computed";
    public override ExportGroupKind Kind
    {
        get { return ExportGroupKind.FastArray; }
    }
    protected override void Configure()
    {
        AddProperty(x => x.Value).Float();
    }
}
'''
            }
        )

        self.assertIn("ComputedKindDescriptor", stderr)
        self.assertIn("ExportGroupKind", stderr)

    def test_class_net_cache_functions_are_not_filtered_by_kind(self):
        """Phases 3b/3c build from ClassNetCacheDescriptor, a separate C#
        hierarchy with no Kind property at all. The filter must not reach
        them."""
        output = self.run_generator(
            {
                "EffectCache.cs": r'''
public sealed class EffectManagerComponentClassNetCacheDescriptor : ClassNetCacheDescriptor<EffectManagerComponentClassNetCacheDescriptor>
{
    public override string Path => "/Script/ShooterGame.EffectManagerComponent_ClassNetCache";
    protected override void Configure()
    {
        AddFunction("MulticastPlayOneShotEffect", "SomePath");
    }
}
'''
            }
        )

        self.assertEqual(
            self.entries(output),
            {
                (
                    "/Script/ShooterGame.EffectManagerComponent_ClassNetCache",
                    "MulticastPlayOneShotEffect",
                ),
            },
        )


class SilentDropTests(GeneratorHarness, unittest.TestCase):
    """Ways a declared field left the table without saying so."""

    def test_an_unknown_primitive_type_is_rejected_not_dropped(self):
        """`.Int64()` is not in PRIMITIVE_TYPES, so the statement fell off the
        end of the type ladder and contributed nothing -- no entry, no counter,
        no message. Adding one method upstream would untype every field that
        uses it and the run would still report success.

        This is the same hazard EXPORT_GROUP_KIND_POLICY already names: an
        unclassified kind is a hard failure, not a default. An unclassified
        TYPE has to be one too.
        """
        stderr = self.run_generator_expecting_failure(
            {
                "WidgetDescriptor.cs": r'''
public sealed class WidgetDescriptor : ExportGroupDescriptor<WidgetDescriptor>
{
    public override string Path => "/Script/ShooterGame.Widget";
    protected override void Configure()
    {
        AddProperty(x => x.Spin).Float();
        AddProperty(x => x.Ticks).Int64();
    }
}
''',
            }
        )
        self.assertIn("Int64", stderr)

    def test_a_known_primitive_still_generates(self):
        """The guard must fire on the unknown method, not on every descriptor."""
        output = self.run_generator(
            {
                "WidgetDescriptor.cs": r'''
public sealed class WidgetDescriptor : ExportGroupDescriptor<WidgetDescriptor>
{
    public override string Path => "/Script/ShooterGame.Widget";
    protected override void Configure()
    {
        AddProperty(x => x.Spin).Float();
    }
}
''',
            }
        )
        self.assertEqual(
            self.entries(output), {("/Script/ShooterGame.Widget", "Spin")}
        )

    @staticmethod
    def widget(*statements: str) -> dict[str, str]:
        """One descriptor on a path nothing else uses, declaring `statements`."""
        body = "\n".join(f"        {statement}" for statement in statements)
        return {
            "WidgetDescriptor.cs": r'''
public sealed class WidgetDescriptor : ExportGroupDescriptor<WidgetDescriptor>
{
    public override string Path => "/Script/ShooterGame.Widget";
    protected override void Configure()
    {
''' + body + r'''
    }
}
''',
        }

    def test_a_declaration_with_no_type_method_is_rejected_not_dropped(self):
        """`AddProperty(x => x.Ticks);` names no type method, so
        `_extract_type_name` returned None and the ladder's last step recorded
        nothing -- no entry, no rejection, no message. Only a NAMED unknown
        method was a failure. Upstream really does write this shape, three
        times (DECODERLESS_PROPERTIES); anywhere else it must stop the run.

        The label is asserted, not just the field name: the report echoes the
        statement, so "Ticks" would appear under any label.
        """
        stderr = self.run_generator_expecting_failure(
            self.widget(
                "AddProperty(x => x.Spin).Float();",
                "AddProperty(x => x.Ticks);",
            )
        )
        self.assertIn("  .<no type method>(): AddProperty(x => x.Ticks);", stderr)

    def test_a_generic_type_method_is_rejected_by_name(self):
        """`.Enum<EMode>()` put a `<` where `_extract_type_name` wanted a `(`,
        so it read as no type method at all and fell off the same way. It must
        fail, and name the method a reader has to add.
        """
        stderr = self.run_generator_expecting_failure(
            self.widget(
                "AddProperty(x => x.Spin).Float();",
                "AddProperty(x => x.Mode).Enum<EMode>();",
            )
        )
        self.assertIn(
            "  .Enum(): AddProperty(x => x.Mode).Enum<EMode>();", stderr
        )

    def test_a_typed_wrapper_definition_is_rejected(self):
        """A helper that returns the builder for its caller to type. Its calls
        (`AddValue(7, x => x.Typed).UInt32();`) do not start with AddProperty,
        so the splitter never sees them; its own body is the one statement
        that shows declarations are routed through it, and that body names no
        type method. Dropping it silently dropped every call with it.
        """
        stderr = self.run_generator_expecting_failure(
            {
                "WrappedDescriptor.cs": r'''
public sealed class WrappedDescriptor : ExportGroupDescriptor<WrappedDescriptor>
{
    public override string Path => "/Script/ShooterGame.Wrapped";
    protected override void Configure()
    {
        AddProperty(x => x.Known).UInt32();
        AddValue(7, x => x.Typed).UInt32();
    }

    private FieldDescriptorBuilder AddValue(
        uint handle,
        Expression<Func<WrappedDescriptor, uint>> property) =>
        AddPropertyHandle(handle, property, ExportCategory.GameState);
}
''',
            }
        )
        self.assertIn(
            "  .<no type method>(): "
            "AddPropertyHandle(handle, property, ExportCategory.GameState);",
            stderr,
        )

    def test_the_generic_method_the_ladder_knows_still_generates(self):
        """The one generic type method the descriptors use keeps its own
        branch: a `RepLayoutDynamicArray<T>()` is an opaque TArray, so Raw,
        not a rejection now that `_extract_type_name` can see generic names.
        """
        output = self.run_generator(
            self.widget(
                "AddProperty(x => x.Spin).Float();",
                "AddPropertyHandle(6, x => x.FloatValues)"
                ".RepLayoutDynamicArray<EffectDataFloat>();",
            )
        )
        self.assertEqual(
            {
                (group, field, field_type.strip())
                for group, field, field_type in ENTRY_RE.findall(output)
            },
            {
                ("/Script/ShooterGame.Widget", "Spin", "FieldType::Float"),
                ("/Script/ShooterGame.Widget", "FloatValues", "FieldType::Raw"),
            },
        )

    #: A real decoder-less declaration: the vendored
    #: AresAbilitySystemComponentDescriptor.cs, trimmed to one typed sibling.
    #: {TYPE} is what follows `x.AresAttributeSet)`.
    DECODERLESS_SOURCE = r'''
public sealed class AresAbilitySystemComponentDescriptor : ExportGroupDescriptor<AresAbilitySystemComponentDescriptor>
{
    public override string Path => "/Script/ShooterGame.AresAbilitySystemComponent";
    public override ExportGroupKind Kind => ExportGroupKind.Component;
    protected override void Configure()
    {
        AddProperty(x => x.Owner).ObjectNetGuid();
        AddProperty(x => x.AresAttributeSet){TYPE};
    }
}
'''

    def test_a_listed_decoderless_declaration_contributes_nothing(self):
        """What DECODERLESS_PROPERTIES allows: no entry -- as before -- with
        the sibling still typed, the run succeeding, and the declaration
        counted by name in the summary instead of vanishing.
        """
        result, output = self.run_generator_process(
            {
                "AresAbilitySystemComponentDescriptor.cs":
                    self.DECODERLESS_SOURCE.replace("{TYPE}", ""),
            }
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.entries(output or ""),
            {("/Script/ShooterGame.AresAbilitySystemComponent", "Owner")},
        )
        self.assertIn(
            "Declared without a type method (no entry): 1\n"
            "  AresAbilitySystemComponentDescriptor.AresAttributeSet\n",
            result.stdout,
        )

    def test_the_decoderless_list_is_scoped_to_its_class(self):
        """The listed property on a class the list does not name is an
        ordinary rejection; the exception is one declaration, not a name.
        """
        stderr = self.run_generator_expecting_failure(
            self.widget("AddProperty(x => x.AresAttributeSet);")
        )
        self.assertIn(
            "  .<no type method>(): AddProperty(x => x.AresAttributeSet);",
            stderr,
        )

    def test_a_listed_property_declared_with_a_type_fails(self):
        """Once upstream gives a listed property a type, the reason recorded
        for it is false. The run says so rather than carry it.
        """
        stderr = self.run_generator_expecting_failure(
            {
                "AresAbilitySystemComponentDescriptor.cs":
                    self.DECODERLESS_SOURCE.replace("{TYPE}", ".ObjectNetGuid()"),
            }
        )
        self.assertIn("DECODERLESS_PROPERTIES", stderr)
        self.assertIn("AresAbilitySystemComponentDescriptor.AresAttributeSet", stderr)

    #: Two descriptor classes, one Path, one field name, two types.
    CONFLICTING_CLASSES = {
        "AlphaDescriptor.cs": r'''
public sealed class AlphaDescriptor : ExportGroupDescriptor<AlphaDescriptor>
{
    public override string Path => "/Script/ShooterGame.Shared";
    protected override void Configure()
    {
        AddProperty(x => x.Contested).Float();
    }
}
''',
        "BetaDescriptor.cs": r'''
public sealed class BetaDescriptor : ExportGroupDescriptor<BetaDescriptor>
{
    public override string Path => "/Script/ShooterGame.Shared";
    protected override void Configure()
    {
        AddProperty(x => x.Contested).Int32();
    }
}
''',
    }

    def test_two_classes_typing_one_field_two_ways_is_rejected(self):
        """Dedup kept the FIRST entry without comparing types, so which type
        shipped was decided by `sorted(class_paths.items())` -- rename a class
        and the table changes. The explicit handle table one loop below already
        refuses the analogous conflict; this is the same rule for types.
        """
        stderr = self.run_generator_expecting_failure(self.CONFLICTING_CLASSES)
        self.assertIn("Contested", stderr)

    def test_two_classes_agreeing_on_one_field_still_dedupes(self):
        """The case the dedup exists for -- parent and child both declaring the
        same field -- stays silent. Only a DISAGREEMENT is a failure.
        """
        agreeing = {
            name: source.replace(".Int32()", ".Float()")
            for name, source in self.CONFLICTING_CLASSES.items()
        }
        output = self.run_generator(agreeing)
        self.assertEqual(
            self.entries(output), {("/Script/ShooterGame.Shared", "Contested")}
        )


if __name__ == "__main__":
    unittest.main()
