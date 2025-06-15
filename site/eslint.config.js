import js from "@eslint/js";
import globals from "globals";
import reactHooks from "eslint-plugin-react-hooks";
import jsxA11y from "eslint-plugin-jsx-a11y";
import reactRefresh from "eslint-plugin-react-refresh";
import tseslint from "typescript-eslint";
import eslintConfigPrettier from "eslint-config-prettier";
import eslintPluginPrettierRecommended from "eslint-plugin-prettier/recommended";

export default tseslint.config(
  { ignores: ["dist"] },
  eslintConfigPrettier,
  eslintPluginPrettierRecommended,
  jsxA11y.flatConfigs.recommended,
  {
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    files: ["**/*.{ts,tsx}"],
    languageOptions: {
      parserOptions: {
        ecmaFeatures: {
          jsx: true,
        },
      },
      ecmaVersion: 2023,
      globals: globals.browser,
    },
    plugins: {
      jsxA11y,
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": [
        "warn",
        { allowConstantExport: true },
      ],
      "eslint-comments/no-unused-disable": 0,
      "prettier/prettier": [
        "error",
        {
          singleQuote: true,
          printWidth: 100,
        },
      ],
      // 'react/jsx-uses-react': 'error',
      // 'react/jsx-uses-vars': 'error',
      "n/no-extraneous-import": 0,
      "n/no-missing-import": 0,
      "n/no-missing-require": 0,
      "n/no-unpublished-require": 0,
      "n/no-unsupported-features": 0,
      "n/no-unsupported-features/es-builtins": 0,
      "n/no-unsupported-features/es-syntax": 0,
      "n/no-unsupported-features/node-builtins": 0,
      "node/no-missing-import": 0,
      "node/no-missing-require": 0,
      "node/no-unpublished-require": 0,
      "node/no-unsupported-features": 0,
      "node/no-unsupported-features/es-builtins": 0,
      "node/no-unsupported-features/es-syntax": 0,
      "node/no-unsupported-features/node-builtins": 0,
      "no-confusing-arrow": 0,
      "no-console": [
        "error",
        {
          allow: ["log", "warn", "error"],
        },
      ],
      "no-shadow": 0,
      "no-template-curly-in-string": 0,
      "no-unused-vars": 0,
      "no-use-before-define": 0,
      "n/prefer-global/process": 0,
      "sort-imports": 0,
      "react/display-name": 0,
      "react/no-unescaped-entities": 0,
      "react/prop-types": 0,
      "react/react-in-jsx-scope": 0,
      "@typescript-eslint/explicit-function-return-type": 0,
      "@typescript-eslint/explicit-module-boundary-types": 0,
      "@typescript-eslint/no-empty-function": 0,
      "@typescript-eslint/no-empty-interface": 0,
      "@typescript-eslint/no-empty-object-type": 0,
      "@typescript-eslint/no-explicit-any": 0,
      "@typescript-eslint/no-non-null-assertion": 0,
      "@typescript-eslint/no-shadow": 1,
      "jsx-a11y/no-noninteractive-element-interactions": 0,
      "jsx-a11y/no-static-element-interactions": 0,
      "jsx-a11y/click-events-have-key-events": 0,
      "jsx-a11y/label-has-associated-control": [
        2,
        {
          // "labelComponents": ["CustomInputLabel"],
          // "labelAttributes": ["label"],
          controlComponents: ["Input"],
          depth: 3,
        },
      ],
      "@typescript-eslint/no-unused-vars": [
        "error",
        {
          args: "after-used",
          argsIgnorePattern: "([aA]ction|^_)",
          caughtErrorsIgnorePattern: "^_",
          destructuredArrayIgnorePattern: "^_",
          varsIgnorePattern: "^_",
          caughtErrors: "none",
          ignoreRestSiblings: true,
        },
      ],
      "@typescript-eslint/no-var-requires": 0,
    },
  },
);
