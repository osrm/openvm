# Axdb Interface

Axdb utilizes Apache DataFusion as its main interface. Users are able to specify SQL queries directly or utilize DataFusion's `LogicalPlanBuilder` to generate a `LogicalPlan` tree of nodes. Under the hood, Axdb converts the `LogicalPlan` tree to a flat execution-order vector of `AxdbNode`s. Axdb then runs `keygen`, `prove`, and `verify` on these nodes once the appropriate functions are called.

## CommittedPage

Contains information in `Axdb`'s `Page` format, with additional required DataFusion `Schema` information for the page. This new format allows us to use Axdb data in DataFusion.

## AxdbNode

An `AxdbNode` contains a pointer to another `AxdbNode` or a DataFusion `TableSource` that is a `CommittedPage`. `AxdbNode`s contain both the operation to execute, a way to store the appropriate cryptographic information, and the output of the operation in the node itself. Operations must be run in the order of `execute`, `keygen`, `prove`, and then `verify`.

## AxdbController

Generates a flattened `AxdbNode` vector from a `LogicalPlan` tree root node and DataFusion `SessionContext`.

## AxdbExpr

Contains a way to convert DataFusion's `Expr` into an `AxdbExpr`, which is a subset of DataFusion's `Expr` since we do not currently support all `Expr`s.

## Running a test

The following test runs `execute`, `keygen`, `prove`, and `verify` on a `[PageScan, Filter]` execution strategy

```bash
cargo test --release --package axdb-interface --test integration -- test_basic_e2e --exact --show-output
```

### Generating a CommittedPage

A `CommittedPage` can be generated by combining a `Page` and a `Schema` together. There are two tests in `tests/generate_page.rs` named `gen_schema()` and `gen_page()` that can generate the `Page` and `Schema` objects and save them to disk. The `committed_page!` macro will create a new `CommittedPage` with the paths to those generated `Page` and `Schema` files.