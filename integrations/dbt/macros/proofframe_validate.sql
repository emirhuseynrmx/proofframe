{% macro proofframe_validate(model_path, contract_path) %}
  {#
    dbt macros cannot safely spawn local processes. This operation deliberately
    emits a command for an orchestrator or CI step to execute after
    dbt has materialized the model artifact.
  #}
  {% set command = "proofframe check " ~ model_path ~ " --contract " ~ contract_path %}
  {% do log(command, info=True) %}
  {{ return("") }}
{% endmacro %}
